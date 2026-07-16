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

// The model selector's data shapes + option-group building now live in the framework-agnostic core
// (RUST-HEADLESS-CORE-1); this crate is the Dioxus adapter that renders them.
use camerata_ui_core::models::{chat_model_groups, ModelOption, ModelsResp};

#[derive(Clone, PartialEq, Debug, serde::Deserialize)]
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
pub(crate) async fn fetch_rules_catalog() -> Option<String> {
    let mut rules: Vec<CorpusRuleLite> =
        reqwest::get(format!("{}/api/corpus-rules", crate::bff_base()))
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

/// Wrapper for `GET /api/development/context`, which returns an OBJECT
/// `{"ok": bool, "units_of_work": [...]}` — NOT a bare array. Deserialising the
/// response directly as `Vec<UowSnapshot>` silently fails (object ≠ array),
/// which previously left development-state grounding empty and invited the model
/// to fabricate. This wrapper matches the server shape exactly.
#[derive(Clone, serde::Deserialize, Default)]
struct DevelopmentContextResponse {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    units_of_work: Vec<UowSnapshot>,
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
    // The dedicated context endpoint returns the OBJECT wrapper
    // `{"ok": true, "units_of_work": [...]}` — parse it as such (see
    // `DevelopmentContextResponse`). Parsing it as a bare array was the bug that
    // silently emptied development-state grounding.
    let dev_url = format!("{}/api/development/context", crate::bff_base());
    if let Ok(resp) = reqwest::get(&dev_url).await {
        if resp.status().is_success() {
            if let Ok(wrapped) = resp.json::<DevelopmentContextResponse>().await {
                if wrapped.ok {
                    return Some(wrapped.units_of_work);
                }
            }
        }
    }
    // Last-resort fallback: the legacy /api/uow endpoint returns a bare array of
    // the full UnitOfWork (a superset of UowSnapshot; serde(default) covers extras).
    reqwest::get(format!("{}/api/uow", crate::bff_base()))
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

// ── Layer 3c/3d: project context (scan results + selected rules) ──────────────

/// Minimal projection of `GET /api/projects/active/context` — only the fields
/// the chat module needs. Extra fields from the server are ignored by serde.
#[derive(Clone, serde::Deserialize, Default)]
struct ProjectContextLite {
    #[serde(default)]
    pub ok: bool,
    /// The active project's name (present when `ok=true`). Surfaced into the
    /// Layer 3 / Layer 3d headers so the assistant names which project it's
    /// grounded in.
    #[serde(default)]
    pub project_name: Option<String>,
    /// The compact scan-results section rendered by `render_scan_results_for_chat`
    /// on the server. Present when a scan has been run; absent otherwise.
    #[serde(default)]
    pub scan_results_section: Option<String>,
    /// The compact selected-rules section rendered by `render_selected_rules_for_chat`
    /// on the server. Present whenever the user has selected at least one rule in the
    /// onboarding draft — available pre-scan, as soon as a selection is made.
    #[serde(default)]
    pub selected_rules_section: Option<String>,
    /// The committed/governing ruleset summary rendered by `build_ruleset_summary`
    /// on the server. Present post-onboard (after rules have been applied to the
    /// project); absent when the project is not yet onboarded or has no applied rules.
    #[serde(default)]
    pub ruleset_summary: Option<String>,
}

/// Fetch the active project's name plus the scan-results section (Layer 3c),
/// the selected-rules section (Layer 3d), and the committed ruleset summary
/// (Layer 3e) from `GET /api/projects/active/context` in a single round-trip.
/// Returns `(project_name, scan_section, selected_rules_section, ruleset_summary)`
/// where each is `None` when absent/empty. Silently degrades: all are `None`
/// when the endpoint is unreachable or there is no active project.
async fn fetch_project_context_sections(
) -> (Option<String>, Option<String>, Option<String>, Option<String>) {
    let ctx: ProjectContextLite = match reqwest::get(format!(
        "{}/api/projects/active/context",
        crate::bff_base()
    ))
    .await
    .ok()
    .and_then(|r| {
        // reqwest::Response::json is async; block via a small sync parse.
        // We're already inside an async context so we just await below.
        Some(r)
    }) {
        Some(r) => match r.json::<ProjectContextLite>().await {
            Ok(v) => v,
            Err(_) => return (None, None, None, None),
        },
        None => return (None, None, None, None),
    };
    if !ctx.ok {
        return (None, None, None, None);
    }
    (
        ctx.project_name.filter(|s| !s.trim().is_empty()),
        ctx.scan_results_section.filter(|s| !s.trim().is_empty()),
        ctx.selected_rules_section.filter(|s| !s.trim().is_empty()),
        ctx.ruleset_summary.filter(|s| !s.trim().is_empty()),
    )
}

/// Fetch only the scan-results section (Layer 3c). Kept for use in unit tests
/// that predate the selected-rules section.
#[cfg(test)]
async fn fetch_project_scan_section() -> Option<String> {
    fetch_project_context_sections().await.1
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

/// Build the unified system prompt from all context layers.
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
/// Additionally:
/// - Layer 3b: pulled issue spine (optional, from GitHub issues pulled this session).
/// - Layer 3c: scan results (optional, from `GET /api/projects/active/context`).
///   Contains severity/status totals, by-rule breakdown, and a capped finding list
///   so the assistant can answer "what are my critical findings?", "which file has
///   the most violations?", etc. The snippet field is NEVER included (server-side
///   guarantee — snippets may contain credential-shaped values).
///
/// The first three layers are stable across turns within a session, giving
/// Anthropic's automatic system-prompt caching a large stable prefix.
/// Layers 3, 3b, and 3c are appended after the stable prefix so they don't
/// disturb the cached portion.
pub(crate) fn unified_system_prompt(
    rules_catalog: &str,
    uow_section: &str,
    pulled_issues_section: Option<&str>,
    finding: Option<&FindingContext>,
    scan_results_section: Option<&str>,
    selected_rules_section: Option<&str>,
    project_ruleset_section: Option<&str>,
    project_name: Option<&str>,
) -> String {
    let not_covered = UNIFIED_NOT_COVERED_PHRASE;
    let mut p = format!(
        "You are Camerata's in-app AI assistant. You answer questions about Camerata's \
         internals (how it works, crates, modules), how to USE Camerata (onboarding, \
         scanning, rules), which governance rules exist and what they mean, the \
         current development state of tracked stories, and the active project's scan \
         findings. You answer from the context layers provided below — ONLY from those \
         layers. CRITICAL: if a question cannot be answered from any of the layers, \
         respond with exactly \"{not_covered}\" followed by a brief statement of what \
         IS available. Never fabricate facts about Camerata's code, architecture, rules, \
         story states, or scan findings that are not present in the layers. Each dynamic \
         layer below (Layers 3, 3c, 3d) ALWAYS appears; when its body reads \"NONE\" the \
         value is definitively empty/zero, NOT unknown — answer accordingly and never \
         infer or invent values from any other layer. In particular, the Layer 2 catalog \
         lists rules that EXIST in Camerata, NOT rules the user has selected; never derive \
         selected-rule names or counts from it. If asked which rules are selected and \
         Layer 3d reads NONE, state plainly that none are currently selected. These \
         dynamic layers are re-read FRESH every turn and reflect the state at THAT moment; \
         the user changes selections, scans, and stories over time, so a value that differs \
         from an earlier turn is a normal live update, NOT a contradiction and NOT a prior \
         mistake — report the current value WITHOUT apologizing for, retracting, or \
         second-guessing earlier answers, and do NOT describe trends or extrapolate (e.g. \
         'a big increase since last time'). When stating a selection count, quote the exact \
         'Total selected: N' figure from Layer 3d; never estimate or round. Be concise and \
         concrete.\n\n"
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
    match project_name {
        Some(name) if !name.trim().is_empty() => {
            p.push_str(&format!(
                "=== LAYER 3: LIVE DEVELOPMENT STATE (project: {name}, refreshed this turn) ===\n"
            ));
        }
        _ => {
            p.push_str(
                "=== LAYER 3: LIVE DEVELOPMENT STATE (all tracked stories, refreshed this turn) ===\n",
            );
        }
    }
    p.push_str(uow_section);
    p.push_str("\n");

    // ── Layer 3b: pulled issue spine (optional) ───────────────────────────────
    // Present when the architect has pulled issues from GitHub this session.
    // Shows Epic → child structure so the model can answer "what issues are open?",
    // "what does #42 track?", "what's under the auth Epic?" etc.
    // Appended after the UoW tail so it doesn't disturb the stable cached prefix.
    if let Some(sec) = pulled_issues_section {
        if !sec.trim().is_empty() {
            p.push_str(
                "=== LAYER 3b: PULLED ISSUES (open GitHub issues pulled this session) ===\n",
            );
            p.push_str(sec);
            p.push_str("\n");
        }
    }

    // ── Layer 3c: active project scan results (optional) ─────────────────────
    // Present when the active project has a scan report. Contains severity/status
    // totals, by-rule breakdown, and a capped finding list. Snippet is NEVER
    // included (server-side guarantee); only rule + location + gate detail appear.
    // Always rendered (absence is meaningful): an empty body must read as an
    // explicit NONE so the model never invents findings.
    p.push_str(
        "=== LAYER 3c: ACTIVE PROJECT SCAN RESULTS (from /api/projects/active/context) ===\n",
    );
    match scan_results_section {
        Some(sec) if !sec.trim().is_empty() => p.push_str(sec),
        _ => p.push_str(
            "NONE — the active project has no scan results (not scanned yet, or zero \
             findings). Do NOT infer or invent any findings.\n",
        ),
    }
    p.push_str("\n");

    // ── Layer 3d: selected rules & options (optional) ────────────────────────
    // Present as soon as the user has selected rules in the onboarding draft —
    // available pre-scan. Shows total selected count, rule ids, and any non-default
    // chosen options. Lets the architect ask "which rules did I select?" at any point.
    // Always rendered (absence is meaningful): an empty body must read as an
    // explicit ZERO so the model never fabricates a selection count from Layer 2.
    match project_name {
        Some(name) if !name.trim().is_empty() => {
            p.push_str(&format!(
                "=== LAYER 3d: SELECTED RULES & OPTIONS (from {name} onboarding draft) ===\n"
            ));
        }
        _ => {
            p.push_str(
                "=== LAYER 3d: SELECTED RULES & OPTIONS (this project, from onboarding draft) ===\n",
            );
        }
    }
    match selected_rules_section {
        Some(sec) if !sec.trim().is_empty() => p.push_str(sec),
        _ => p.push_str(
            "NONE — the user currently has ZERO rules selected for this project. Do NOT \
             infer, estimate, or invent any selected-rule names or counts (the Layer 2 \
             catalog lists rules that EXIST, not rules the user selected). If asked which \
             rules are selected, state plainly that none are currently selected.\n",
        ),
    }
    p.push_str("\n");

    // ── Layer 3e: project committed ruleset (optional) ───────────────────────
    // The project's APPLIED/governing ruleset (post-onboard), distinct from the
    // Layer 3d onboarding-draft selection and the Layer 2 catalog of all rules
    // that exist. Always rendered (absence is meaningful): an empty body must
    // read as an explicit NONE so the model never infers the governing rules
    // from the Layer 2 catalog.
    p.push_str(
        "=== LAYER 3e: PROJECT RULESET (committed rules governing this project) ===\n",
    );
    match project_ruleset_section {
        Some(sec) if !sec.trim().is_empty() => p.push_str(sec),
        _ => p.push_str(
            "NONE — no committed ruleset for this project yet (not onboarded, or no \
             rules applied). Do NOT infer the governing rules from the Layer 2 catalog.\n",
        ),
    }
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

/// Wire shape from `GET /api/models/registry`.
#[derive(serde::Deserialize)]
struct RegistryEntryWire {
    id: String,
    display: String,
    #[serde(default)]
    provider: String,
    #[serde(default)]
    free: bool,
    #[serde(default)]
    tool_use: bool,
    #[serde(default)]
    context: u64,
    #[serde(default)]
    price_out: f64,
    #[serde(default)]
    caching: bool,
}

#[derive(serde::Deserialize)]
struct RegistryResp {
    models: Vec<RegistryEntryWire>,
}

async fn fetch_models() -> Option<ModelsResp> {
    let resp: RegistryResp = reqwest::get(format!("{}/api/models/registry", crate::bff_base()))
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    let models: Vec<ModelOption> = resp
        .models
        .into_iter()
        .map(|e| {
            let mut parts = Vec::<String>::new();
            // Price: FREE or $<price_out>/M.
            if e.free {
                parts.push("FREE".to_string());
            } else if e.price_out > 0.0 {
                let formatted = if e.price_out >= 10.0 {
                    format!("${:.0}/M", e.price_out)
                } else if e.price_out >= 1.0 {
                    let s = format!("{:.1}", e.price_out);
                    format!("${}/M", s.trim_end_matches('0').trim_end_matches('.'))
                } else {
                    let s = format!("{:.2}", e.price_out);
                    format!("${}/M", s.trim_end_matches('0').trim_end_matches('.'))
                };
                parts.push(formatted);
            }
            // Tool-use.
            if e.tool_use {
                parts.push("tool-use".to_string());
            } else {
                parts.push("no-tools".to_string());
            }
            // Context.
            if e.context > 0 {
                parts.push(format!("{}K", e.context / 1000));
            }
            // Caching.
            if e.caching {
                parts.push("cache".to_string());
            }
            let label = if parts.is_empty() {
                e.display.clone()
            } else {
                format!("{}  {}", e.display, parts.join(" · "))
            };
            ModelOption { label, id: e.id, provider: e.provider }
        })
        .collect();

    let default = models
        .iter()
        .find(|m| m.provider == "claude")
        .or_else(|| models.first())
        .map(|m| m.id.clone())
        .unwrap_or_default();

    // `backend` not returned by the registry endpoint; leave empty (shown only when non-empty).
    Some(ModelsResp { models, default, backend: String::new() })
}

/// The subset of `GET /api/settings` the chat selector needs: the APP-LEVEL (cross-project)
/// chat assistant model. The chat is a global assistant, so its model is an app setting.
#[derive(serde::Deserialize)]
struct SettingsLite {
    #[serde(default)]
    chat_model: Option<String>,
}

/// Fetch the app-level chat assistant model from `GET /api/settings`. `None` when unset/blank
/// or the server is unreachable.
async fn fetch_app_chat_model() -> Option<String> {
    let s: SettingsLite = reqwest::get(format!("{}/api/settings", crate::bff_base()))
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    s.chat_model.filter(|m| !m.trim().is_empty())
}

/// Persist the app-level chat assistant model via `POST /api/settings/chat-model`. Best-effort.
async fn save_app_chat_model(model: &str) -> bool {
    let body = serde_json::json!({ "model": model });
    reqwest::Client::new()
        .post(format!("{}/api/settings/chat-model", crate::bff_base()))
        .json(&body)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// Capture a chat reply as a project-memory learning (#112): resolve the active project, then POST
/// the text as a human-curated (Approved) entry. Returns true on success (false when there is no
/// active project or the request fails).
async fn add_chat_learning(text: &str) -> bool {
    #[derive(serde::Deserialize)]
    struct ActiveId {
        id: String,
    }
    let base = crate::bff_base();
    let resp = match reqwest::get(format!("{base}/api/projects/active")).await {
        Ok(r) => r,
        Err(_) => return false,
    };
    let Some(active) = resp.json::<Option<ActiveId>>().await.ok().flatten() else {
        return false;
    };
    reqwest::Client::new()
        .post(format!("{base}/api/projects/{}/memory", active.id))
        .json(&serde_json::json!({ "kind": "decision", "text": text }))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

/// A prior chat turn sent to the server so the model has conversation context.
/// Mirrors `ChatTurn` in `crates/server/src/lib.rs`; role is "user" or "assistant".
#[derive(Clone, serde::Serialize)]
struct ChatHistoryTurn {
    role: &'static str,
    content: String,
}

/// Build the history payload from the current turns signal.
/// Converts the local `Turn` (role = "you" | "ai") into the server-facing
/// `ChatHistoryTurn` (role = "user" | "assistant"). The new message being sent
/// is NOT included — that is `prompt` on the request.
fn turns_to_history(turns: &[Turn]) -> Vec<ChatHistoryTurn> {
    turns
        .iter()
        .map(|t| ChatHistoryTurn {
            role: if t.role == "you" { "user" } else { "assistant" },
            content: t.text.clone(),
        })
        .collect()
}

async fn send_chat(
    prompt: &str,
    model: &str,
    system: &str,
    history: Vec<ChatHistoryTurn>,
) -> Result<ChatResp, String> {
    let body = serde_json::json!({
        "prompt": prompt,
        "model": model,
        "system": system,
        "history": history,
    });
    let resp = reqwest::Client::new()
        .post(format!("{}/api/chat", crate::bff_base()))
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("request to /api/chat failed: {e}"))?;
    let status = resp.status();
    // Read the body as text first so a non-2xx error body (e.g. the real `claude`
    // CLI failure the server surfaces) is preserved instead of being swallowed by
    // a failed `ChatResp` deserialize. This is the difference between the user
    // seeing the actual error and seeing a generic "(no response)".
    let text = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(format!("backend error {status}: {text}"));
    }
    serde_json::from_str::<ChatResp>(&text)
        .map_err(|e| format!("could not parse chat response ({e}); raw body: {text}"))
}

/// Decide the assistant turn text from a `send_chat` outcome. Pure (no I/O) so it
/// is unit-testable: a real backend error is now shown verbatim instead of a
/// generic "no response" line that hid the cause.
fn chat_reply_text(reply: Result<ChatResp, String>) -> String {
    match reply {
        Ok(r) if !r.text.trim().is_empty() => r.text,
        Ok(_) => "(the model returned an empty response)".to_string(),
        Err(e) => format!("\u{26a0} chat failed — {e}"),
    }
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
    /// Pre-rendered issue spine injected by the caller from the app-lifetime
    /// `GOV_DEV_STATE` pull cache. When `Some`, this becomes Layer 3b of the
    /// system prompt. The caller computes the text; `ChatBubble` injects it
    /// opaquely, keeping the chat module free of WorkItem dependencies.
    #[props(default)]
    pub pulled_issues_section: Option<String>,
}

#[component]
pub fn ChatBubble(props: ChatBubbleProps) -> Element {
    let mut open = use_signal(|| false);
    let models_res = use_resource(fetch_models);
    let models = models_res.read().clone().flatten();

    // The chat assistant is GLOBAL: its model is an APP-LEVEL (cross-project) setting, fetched
    // from `GET /api/settings`. The selector seeds from it (falling back to the registry default
    // when unset) and persists changes back via `POST /api/settings/chat-model`. The per-request
    // `model` sent in the chat body remains the explicit, highest-precedence override server-side.
    let app_chat_model_res = use_resource(fetch_app_chat_model);
    // The registry default, used only as a placeholder while the app-level model loads.
    let default_model = models
        .as_ref()
        .map(|m| m.default.clone())
        .filter(|d| !d.is_empty());

    let mut model = use_signal(String::new);
    // True once the user picks a model in this chat session, so the seeding effect below
    // stops overriding their choice.
    let mut user_override = use_signal(|| false);
    // Seed via an effect (NOT during render) so it runs AFTER the app-level `chat_model`
    // resource resolves. The previous render-body seed raced the resource: it set the
    // registry default on the first paint (while the fetch was pending), then skipped
    // re-seeding because `model` was no longer empty — so the SAVED app-level model was
    // never adopted and the in-box selector appeared not to reflect / apply the chosen model.
    // Now: adopt the app-level model whenever it is known (unless the user has picked since),
    // falling back to the registry default only as a placeholder while the fetch is pending.
    {
        let default_model = default_model.clone();
        use_effect(move || {
            let app = app_chat_model_res
                .read()
                .clone()
                .flatten()
                .filter(|m| !m.trim().is_empty());
            if let Some(m) = app {
                if !user_override() {
                    model.set(m);
                }
            } else if model().is_empty() {
                if let Some(d) = &default_model {
                    model.set(d.clone());
                }
            }
        });
    }
    let backend = models
        .as_ref()
        .map(|m| m.backend.clone())
        .unwrap_or_default();

    let mut turns = use_signal(Vec::<Turn>::new);
    let mut draft = use_signal(String::new);
    let mut sending = use_signal(|| false);
    // Toast surface, for the "Add to learnings" affordance on AI replies (#112).
    let toasts = use_context::<Signal<Vec<crate::toast::Toast>>>();
    // Collapsed by default: the "what this assistant can see" strip shows the top few items + a
    // see-more/less toggle so it doesn't eat a chunk of the transcript's vertical space.
    let mut ctx_expanded = use_signal(|| false);

    // Layer 2: rules catalog — fetched once per session, fed into the static
    // prefix of the unified system prompt.
    // The rules catalog is fetched once at app scope (main.rs) and shared via context, so it is
    // available here regardless of how often this bubble mounts. See the provider in App.
    let rules_res = use_context::<Resource<Option<String>>>();
    let rules_catalog = rules_res.read().clone().flatten().unwrap_or_default();

    // Layer 3: UoW snapshot — fetched per turn (when the panel is open and a
    // message is sent). Also pre-fetched when the panel opens so the "what this
    // assistant can see" strip can show a story count without the user needing
    // to send a message first.
    // A periodic tick drives a LIVE refresh of the status strip below while the
    // chat panel is open, so it reflects current server state (selections / scan /
    // stories change as the user works) instead of a frozen session-start snapshot.
    // Gated on `open` so it does not poll localhost in the background when closed.
    let mut refresh_tick = use_signal(|| 0u32);
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
            if open() {
                *refresh_tick.write() += 1;
            }
        }
    });

    // Layer 3: UoW snapshot — re-fetched on each tick (and once on mount). Reading
    // `refresh_tick()` inside the closure registers the dependency that re-runs it.
    let uow_res = use_resource(move || {
        let _ = refresh_tick();
        fetch_uow_snapshot()
    });
    let uow_snaps: Vec<UowSnapshot> = uow_res.read().clone().flatten().unwrap_or_default();
    // `Some(_)` means the fetch RESOLVED (even to empty); `None` means still pending.
    // Lets the strip distinguish "loading" from "loaded but genuinely empty".
    let uow_resolved = uow_res.read().is_some();

    // Layers 3c + 3d (+ project name): scan results, selected-rules sections, and
    // the active project name — fetched together from the active project context
    // endpoint, also re-fetched on each tick so the strip stays live.
    //
    // NOTE: these reads ONLY feed the "what this assistant can see" status strip
    // below. The send paths (onkeydown / onclick) refetch this context fresh
    // inside their spawn so each message reflects the latest selection / project /
    // scan independently of this strip.
    let ctx_res = use_resource(move || {
        let _ = refresh_tick();
        fetch_project_context_sections()
    });
    let (_active_project_name, scan_section, selected_rules_section, ruleset_summary): (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    ) = ctx_res.read().clone().unwrap_or((None, None, None, None));

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
    // Clone the session-static rules catalog (Layer 2, fetched once per session)
    // for the two send closures (onkeydown + onclick each move-capture). The UoW
    // snapshot (Layer 3), scan results (Layer 3c), selected rules (Layer 3d), and
    // active project name are NOT captured here — each send refetches them fresh
    // inside its spawn so a message always reflects the live state.
    let catalog_kd = rules_catalog.clone();
    let catalog_btn = rules_catalog;

    // The number of rows the "see more" toggle reveals (uow state, pulled issues, scan
    // results, selected rules, ruleset) — named so the toggle label can't silently drift
    // from the actual row count if a row is added/removed below.
    const CTX_EXTRA_ROWS: usize = 5;

    rsx! {
        // Floating launcher — reuses the same amber square FAB as the rest of the app.
        button {
            class: "chat-fab",
            title: "Camerata assistant (AI)",
            onclick: move |_| open.toggle(),
            if open() { "✕" } else { "💬" }
        }

        if open() {
            div {
                // .chat-panel already carries position/size/dark-surface (var(--surface))/
                // border/shadow — the fixed height (not just max-height) means .chat-log's
                // flex:1 below resolves against a definite height, so it scrolls reliably
                // with no inline max-height/!important hack needed.
                class: "chat-panel",

                // ── header ──────────────────────────────────────────────────
                div {
                    class: "chat-head",
                    span { class: "chat-title", "Camerata assistant" }
                    select {
                        class: "chat-model",
                        value: "{model}",
                        onchange: move |e| {
                            let chosen = e.value();
                            model.set(chosen.clone());
                            // Mark a manual choice so the seeding effect stops re-applying
                            // the app-level value over the user's selection this session.
                            user_override.set(true);
                            // Persist the choice as the app-level (cross-project) chat model.
                            spawn(async move {
                                save_app_chat_model(&chosen).await;
                            });
                        },
                        // Resilient: chat_model_groups ALWAYS yields at least the current
                        // model, so this selector can never render empty/invisible (the
                        // recurring "model selector disappeared" bug). Guarded by the
                        // chat_model_groups_* unit tests.
                        for (group_label , opts) in chat_model_groups(&models, &model()).into_iter() {
                            optgroup { label: "{group_label}",
                                for opt in opts.into_iter() {
                                    option { key: "{opt.id}", value: "{opt.id}", "{opt.label}" }
                                }
                            }
                        }
                    }
                    if !backend.is_empty() {
                        span { class: "chat-backend", "{backend}" }
                    }
                }

                // ── "what this assistant can see" affordance ─────────────
                div {
                    class: "chat-context",
                    div { class: "chat-context-title", "What this assistant can see:" }
                    div { class: "chat-context-list",
                        div { class: "chat-context-row",
                            span { class: "chat-context-dot on", "●" }
                            span { "Technical reference (docs/TECHNICAL.md)" }
                        }
                        div { class: "chat-context-row",
                            span { class: "chat-context-dot on", "●" }
                            span { "User guide (docs/USER_GUIDE.md)" }
                        }
                        div { class: "chat-context-row",
                            span {
                                class: if rules_catalog_loaded(&static_prefix_catalog) {
                                    "chat-context-dot on"
                                } else {
                                    "chat-context-dot"
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
                        // The rest of the context items collapse behind "see more". Always
                        // exactly CTX_EXTRA_ROWS rows (uow/pulled-issues/scan/selected-rules/
                        // ruleset), so the toggle label below can't drift out of sync with them.
                        if ctx_expanded() {
                        {
                            let uow_label = if !uow_snaps.is_empty() {
                                format!("Development state ({} stories, live)", uow_snaps.len())
                            } else if uow_resolved {
                                "Development state (no stories tracked yet)".to_string()
                            } else {
                                "Development state (loading\u{2026})".to_string()
                            };
                            // Lit once resolved (even if empty); dim only while pending.
                            let uow_dot_cls = if uow_resolved { "chat-context-dot on" } else { "chat-context-dot" };
                            rsx! {
                                div { class: "chat-context-row",
                                    span { class: "{uow_dot_cls}", "\u{25cf}" }
                                    span { "{uow_label}" }
                                }
                            }
                        }
                        // Layer 3b: pulled issues indicator — shown when GitHub issues
                        // have been pulled into the session and handed to the assistant.
                        {
                            let pis = props.pulled_issues_section.as_deref().filter(|s| !s.trim().is_empty());
                            let pis_dot_cls = if pis.is_some() { "chat-context-dot on" } else { "chat-context-dot" };
                            let pis_label = if let Some(sec) = pis {
                                // Count issue lines: an issue spine renders each item on a line
                                // starting with "#" (e.g. "#42 …") or "- " (Epic/child bullets).
                                let n = sec
                                    .lines()
                                    .map(|l| l.trim_start())
                                    .filter(|l| l.starts_with('#') || l.starts_with("- "))
                                    .count();
                                if n > 0 {
                                    format!("Pulled issues ({n})")
                                } else {
                                    "Pulled issues (loaded)".to_string()
                                }
                            } else {
                                "Pulled issues (none pulled yet)".to_string()
                            };
                            rsx! {
                                div { class: "chat-context-row",
                                    span { class: "{pis_dot_cls}", "\u{25cf}" }
                                    span { "{pis_label}" }
                                }
                            }
                        }
                        // Layer 3c: scan results indicator — shown when a scan has been run.
                        {
                            let scan_dot_cls = if scan_section.is_some() { "chat-context-dot on" } else { "chat-context-dot" };
                            let scan_label = if scan_section.is_some() {
                                "Scan results (active project, live)"
                            } else {
                                "Scan results (none yet — run a scan to populate)"
                            };
                            rsx! {
                                div { class: "chat-context-row",
                                    span { class: "{scan_dot_cls}", "\u{25cf}" }
                                    span { "{scan_label}" }
                                }
                            }
                        }
                        // Layer 3d: selected rules — available pre-scan, from the onboarding draft.
                        {
                            let sel_dot_cls = if selected_rules_section.is_some() { "chat-context-dot on" } else { "chat-context-dot" };
                            let sel_label = if let Some(ref sec) = selected_rules_section {
                                // Extract the count from the first line "Total selected: N rule(s)".
                                let n = sec
                                    .lines()
                                    .next()
                                    .and_then(|l| l.strip_prefix("Total selected: "))
                                    .and_then(|rest| rest.split_whitespace().next())
                                    .and_then(|n| n.parse::<usize>().ok())
                                    .unwrap_or(0);
                                format!("Selected rules ({n} selected)")
                            } else {
                                "Selected rules (none yet)".to_string()
                            };
                            rsx! {
                                div { class: "chat-context-row",
                                    span { class: "{sel_dot_cls}", "\u{25cf}" }
                                    span { "{sel_label}" }
                                }
                            }
                        }
                        // Layer 3e: committed ruleset indicator — present post-onboard,
                        // once the project's governing rules have been applied.
                        {
                            let rs_dot_cls = if ruleset_summary.is_some() { "chat-context-dot on" } else { "chat-context-dot" };
                            let rs_label = if ruleset_summary.is_some() {
                                "Project ruleset (committed)"
                            } else {
                                "Project ruleset (none yet)"
                            };
                            rsx! {
                                div { class: "chat-context-row",
                                    span { class: "{rs_dot_cls}", "\u{25cf}" }
                                    span { "{rs_label}" }
                                }
                            }
                        }
                        }
                        // Layer 4: only shown when a finding is focused.
                        if let Some(ref f) = *active_finding.read() {
                            if !f.rule_id.is_empty() {
                                div {
                                    class: "chat-context-finding",
                                    span { class: "chat-context-finding-icon", "◆" }
                                    span { "Focused finding: " }
                                    span { class: "mono", "{f.rule_id}" }
                                    span { " {f.path}:{f.line}" }
                                }
                            }
                        }
                        button {
                            class: "chat-context-toggle",
                            onclick: move |_| ctx_expanded.toggle(),
                            if ctx_expanded() { "see less" } else { "see more ({CTX_EXTRA_ROWS} more)" }
                        }
                    }
                }

                // ── transcript ──────────────────────────────────────────────
                div {
                    class: "chat-log",
                    if turns().is_empty() {
                        p {
                            class: "chat-empty",
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
                            class: if t.role == "you" { "chat-turn you" } else { "chat-turn ai" },
                            if t.role == "ai" {
                                div {
                                    class: "chat-turn-text md chat-ai-md",
                                    dangerous_inner_html: md_to_html(&t.text)
                                }
                                button {
                                    class: "chat-add-learning",
                                    title: "Add this reply to project memory",
                                    onclick: {
                                        let txt = t.text.clone();
                                        move |_| {
                                            let txt = txt.clone();
                                            spawn(async move {
                                                let ok = add_chat_learning(&txt).await;
                                                crate::toast::push_toast(
                                                    toasts,
                                                    if ok { crate::toast::ToastKind::Info } else { crate::toast::ToastKind::Error },
                                                    if ok { "Added to project memory." } else { "No active project, or the add failed." },
                                                );
                                            });
                                        }
                                    },
                                    "+ Add to learnings"
                                }
                            } else {
                                div { class: "chat-turn-text", "{t.text}" }
                            }
                        }
                    }
                    if sending() {
                        div { class: "chat-turn ai",
                            div { class: "chat-turn-text dim", "thinking…" }
                        }
                    }
                }

                // ── compose bar ─────────────────────────────────────────────
                div {
                    class: "chat-compose",
                    textarea {
                        class: "chat-input",
                        rows: "2",
                        placeholder: "Ask anything about Camerata… (Enter to send, Shift+Enter for newline)",
                        value: "{draft}",
                        onkeydown: {
                            let catalog_kd2 = catalog_kd.clone();
                            let finding_kd = active_finding.read().clone();
                            let pis_kd = props.pulled_issues_section.clone();
                            move |e: Event<KeyboardData>| {
                                if e.key() == Key::Enter && !e.modifiers().shift() {
                                    e.prevent_default();
                                    let prompt = draft().trim().to_string();
                                    if prompt.is_empty() || sending() {
                                        return;
                                    }
                                    let mdl = model();
                                    // Snapshot prior turns BEFORE appending the new user message —
                                    // the server renders history + the new prompt separately.
                                    let history = turns_to_history(&turns.read());
                                    turns.write().push(Turn { role: "you", text: prompt.clone() });
                                    draft.set(String::new());
                                    sending.set(true);
                                    let catalog_send = catalog_kd2.clone();
                                    let finding_send = finding_kd.clone();
                                    let pis_send = pis_kd.clone();
                                    spawn(async move {
                                        // The Bombe is reserved for AI work: hold a loading guard
                                        // for the whole chat turn so the machine runs while the
                                        // assistant is thinking and stops when the reply returns.
                                        let _guard = crate::loading::LoadingGuard::new();
                                        // Per-turn refetch: pull the LIVE dev state + project context
                                        // (scan results, selected rules, project name) so the prompt
                                        // reflects the user's CURRENT selection / project / story state
                                        // rather than the snapshot taken when the chat was opened.
                                        let uow_snaps =
                                            fetch_uow_snapshot().await.unwrap_or_default();
                                        let (project_name, scan_section, rules_section, ruleset_section) =
                                            fetch_project_context_sections().await;
                                        let uow_sec = render_uow_section(&uow_snaps);
                                        let sys = unified_system_prompt(
                                            &catalog_send,
                                            &uow_sec,
                                            pis_send.as_deref(),
                                            finding_send.as_ref(),
                                            scan_section.as_deref(),
                                            rules_section.as_deref(),
                                            ruleset_section.as_deref(),
                                            project_name.as_deref(),
                                        );
                                        let reply = send_chat(&prompt, &mdl, &sys, history).await;
                                        sending.set(false);
                                        turns.write().push(Turn { role: "ai", text: chat_reply_text(reply) });
                                    });
                                }
                            }
                        },
                        oninput: move |e| draft.set(e.value()),
                    }
                    div {
                        class: "chat-send-col",
                        button {
                            // .chat-send:disabled already dims (opacity .5) + sets cursor:not-allowed —
                            // the old inline `opacity: if sending() {"0.5"}...` baked the literal Rust
                            // `if {} else {}` text into the CSS string (no `{}` interpolation), so it
                            // was never valid CSS and the button never visibly dimmed.
                            class: "chat-send",
                            disabled: sending() || draft().trim().is_empty(),
                            onclick: {
                                let catalog_btn2 = catalog_btn.clone();
                                let finding_btn = active_finding.read().clone();
                                let pis_btn = props.pulled_issues_section.clone();
                                move |_| {
                                    let prompt = draft().trim().to_string();
                                    if prompt.is_empty() || sending() {
                                        return;
                                    }
                                    let mdl = model();
                                    // Snapshot prior turns BEFORE appending the new user message —
                                    // the server renders history + the new prompt separately.
                                    let history = turns_to_history(&turns.read());
                                    turns.write().push(Turn { role: "you", text: prompt.clone() });
                                    draft.set(String::new());
                                    sending.set(true);
                                    let catalog_send = catalog_btn2.clone();
                                    let finding_send = finding_btn.clone();
                                    let pis_send = pis_btn.clone();
                                    spawn(async move {
                                        // The Bombe is reserved for AI work: hold a loading guard
                                        // for the whole chat turn so the machine runs while the
                                        // assistant is thinking and stops when the reply returns.
                                        let _guard = crate::loading::LoadingGuard::new();
                                        // Per-turn refetch: pull the LIVE dev state + project context
                                        // (scan results, selected rules, project name) so the prompt
                                        // reflects the user's CURRENT selection / project / story state
                                        // rather than the snapshot taken when the chat was opened.
                                        let uow_snaps =
                                            fetch_uow_snapshot().await.unwrap_or_default();
                                        let (project_name, scan_section, rules_section, ruleset_section) =
                                            fetch_project_context_sections().await;
                                        let uow_sec = render_uow_section(&uow_snaps);
                                        let sys = unified_system_prompt(
                                            &catalog_send,
                                            &uow_sec,
                                            pis_send.as_deref(),
                                            finding_send.as_ref(),
                                            scan_section.as_deref(),
                                            rules_section.as_deref(),
                                            ruleset_section.as_deref(),
                                            project_name.as_deref(),
                                        );
                                        let reply = send_chat(&prompt, &mdl, &sys, history).await;
                                        sending.set(false);
                                        turns.write().push(Turn { role: "ai", text: chat_reply_text(reply) });
                                    });
                                }
                            },
                            "Send"
                        }
                        button {
                            class: "chat-clear-btn",
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
        chat_reply_text, render_uow_section, unified_system_prompt, ChatResp,
        DevelopmentContextResponse, FindingContext, GateProvenanceLite, UowSnapshot, TECHNICAL_DOC,
        UNIFIED_NOT_COVERED_PHRASE, USER_GUIDE,
    };

    // ── in-chatbox model selector: it must NEVER render empty/invisible ────────
    // (this selector has regressed away repeatedly; these guard the option-building logic).

    // `CAMERATA_BFF_URL` is a process-global env var. Every Tier-2 test below sets it (to a
    // mock-server URI) then removes it. `cargo test` runs tests on parallel threads, so two such
    // tests overlapping would clobber each other's override and point a helper at the wrong server.
    // This mutex serializes the env-mutating tests against each other (the doc's "serial_test-style
    // mutex"; we can't add the serial_test crate without touching Cargo.toml). Lock for the whole
    // body; recover from poisoning so one failing test doesn't cascade-fail the rest.
    static BFF_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn bff_env_guard() -> std::sync::MutexGuard<'static, ()> {
        BFF_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ── Tier-2 UI test: a network helper against a MOCK BFF (wiremock) ──────────
    // Verifies add_chat_learning's request CONTRACT: it GETs the active project, then POSTs the
    // reply text to that project's /memory with the right body. Points the helper at a fake server
    // via the CAMERATA_BFF_URL seam. (The env override is process-global; this is the only test that
    // reads bff_base(), so it can't race another helper.)
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn add_chat_learning_resolves_active_then_posts_the_reply() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/active"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "id": "proj-7", "name": "Acme" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/projects/proj-7/memory"))
            .and(body_json(
                serde_json::json!({ "kind": "decision", "text": "A durable learning." }),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::add_chat_learning("A durable learning.").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "resolves the active project, then posts the learning");
        // `.expect(1)` on the POST mock asserts (on server drop) it was hit once with the exact
        // path + body — i.e. the helper sent {kind, text} to /api/projects/proj-7/memory.
    }

    // ── Tier-2: send_chat POSTs the right body to /api/chat ───────────────────
    // The request CONTRACT for the chat turn is load-bearing: a wrong field name (prompt / model /
    // system / history) silently breaks grounding or model selection. body_json asserts every field.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn send_chat_posts_prompt_model_system_and_history() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .and(body_json(serde_json::json!({
                "prompt": "What is CAM-1 blocked on?",
                "model": "claude-opus-4-8",
                "system": "SYS PROMPT",
                "history": [
                    { "role": "user", "content": "hi" },
                    { "role": "assistant", "content": "hello" },
                ],
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "text": "It is blocked on the gate.", "backend": "cli" })),
            )
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let history = vec![
            super::ChatHistoryTurn { role: "user", content: "hi".to_string() },
            super::ChatHistoryTurn { role: "assistant", content: "hello".to_string() },
        ];
        let res = super::send_chat("What is CAM-1 blocked on?", "claude-opus-4-8", "SYS PROMPT", history).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let resp = res.expect("2xx with a valid ChatResp body parses");
        assert_eq!(resp.text, "It is blocked on the gate.");
        assert_eq!(resp.backend, "cli");
    }

    // send_chat must surface a non-2xx body verbatim (the difference between the user seeing the
    // real `claude` CLI error and a generic "no response"). Asserts both the status and the body
    // text make it into the Err string.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn send_chat_surfaces_backend_error_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(ResponseTemplate::new(500).set_body_string("claude CLI exited 1: rate limited"))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let res = super::send_chat("hi", "m", "s", Vec::new()).await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let err = res.expect_err("a 500 must be an Err");
        assert!(err.contains("500"), "error must include the status; got: {err}");
        assert!(
            err.contains("claude CLI exited 1: rate limited"),
            "error must preserve the backend body verbatim; got: {err}"
        );
    }

    // ── Tier-2: save_app_chat_model POSTs {model} to /api/settings/chat-model ──
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn save_app_chat_model_posts_model_body() {
        use wiremock::matchers::{body_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/chat-model"))
            .and(body_json(serde_json::json!({ "model": "claude-sonnet-4" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": true })))
            .expect(1)
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::save_app_chat_model("claude-sonnet-4").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(ok, "a 2xx response must be reported as success");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn save_app_chat_model_reports_failure_on_non_2xx() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/settings/chat-model"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let ok = super::save_app_chat_model("x").await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert!(!ok, "a 500 response must be reported as failure (best-effort helper)");
    }

    // ── Tier-2: fetch_app_chat_model GETs /api/settings and reads chat_model ──
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_app_chat_model_parses_chat_model_field() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/settings"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "chat_model": "claude-opus-4-8" })),
            )
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let model = super::fetch_app_chat_model().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(model, Some("claude-opus-4-8".to_string()));
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_app_chat_model_returns_none_for_blank_model() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/settings"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "chat_model": "   " })),
            )
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let model = super::fetch_app_chat_model().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(model, None, "a blank/whitespace chat_model must read as unset");
    }

    // ── Tier-2: fetch_models GETs /api/models/registry and builds labels ──────
    // Asserts the registry-entry → ModelOption transformation: provider passes through and the
    // label is composed (price · tool-use · context · cache). A wrong field here makes the selector
    // mislabel models.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_models_parses_registry_and_builds_labels() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/models/registry"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [
                    {
                        "id": "claude-opus-4-8",
                        "display": "Opus",
                        "provider": "claude",
                        "free": false,
                        "tool_use": true,
                        "context": 200000,
                        "price_out": 15.0,
                        "caching": true
                    },
                    {
                        "id": "ds-free",
                        "display": "DeepSeek",
                        "provider": "openrouter",
                        "free": true,
                        "tool_use": false,
                        "context": 64000,
                        "price_out": 0.0,
                        "caching": false
                    }
                ]
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let resp = super::fetch_models().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let resp = resp.expect("registry response must parse into ModelsResp");
        assert_eq!(resp.models.len(), 2);
        // The first claude model is the default.
        assert_eq!(resp.default, "claude-opus-4-8");
        let opus = resp.models.iter().find(|m| m.id == "claude-opus-4-8").expect("opus present");
        assert_eq!(opus.provider, "claude");
        // price_out 15 -> "$15/M"; tool_use -> "tool-use"; 200000 ctx -> "200K"; caching -> "cache".
        assert!(opus.label.contains("Opus"), "label keeps display name; got {}", opus.label);
        assert!(opus.label.contains("$15/M"), "label encodes price; got {}", opus.label);
        assert!(opus.label.contains("tool-use"), "label encodes tool-use; got {}", opus.label);
        assert!(opus.label.contains("200K"), "label encodes context; got {}", opus.label);
        assert!(opus.label.contains("cache"), "label encodes caching; got {}", opus.label);
        let ds = resp.models.iter().find(|m| m.id == "ds-free").expect("deepseek present");
        assert!(ds.label.contains("FREE"), "free models are labelled FREE; got {}", ds.label);
        assert!(ds.label.contains("no-tools"), "non-tool models are labelled no-tools; got {}", ds.label);
    }

    // ── Tier-2: fetch_rules_catalog GETs /api/corpus-rules and renders a catalog ──
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_rules_catalog_renders_sorted_catalog_lines() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/corpus-rules"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                {
                    "id": "SEC-1",
                    "title": "no hardcoded secrets",
                    "domain": "security",
                    "scope": "repo-local",
                    "options": [ { "label": "warn" }, { "label": "deny" } ]
                },
                {
                    "id": "ARCH-1",
                    "title": "layering",
                    "domain": "architecture",
                    "scope": "all-repos",
                    "options": []
                }
            ])))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let catalog = super::fetch_rules_catalog().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let catalog = catalog.expect("non-empty rule corpus yields Some(catalog)");
        assert!(catalog.contains("SEC-1"), "catalog must name the rule id; got:\n{catalog}");
        assert!(catalog.contains("[security · repo-local]"), "catalog encodes domain · scope; got:\n{catalog}");
        assert!(catalog.contains("no hardcoded secrets"), "catalog includes the title");
        assert!(catalog.contains("alternatives: warn / deny"), "catalog lists option labels as alternatives");
        // Sorted by (domain, id): architecture sorts before security.
        let arch_pos = catalog.find("ARCH-1").expect("ARCH-1 present");
        let sec_pos = catalog.find("SEC-1").expect("SEC-1 present");
        assert!(arch_pos < sec_pos, "rules must be sorted by domain then id (architecture before security)");
    }

    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_rules_catalog_returns_none_for_empty_corpus() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/corpus-rules"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let catalog = super::fetch_rules_catalog().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(catalog, None, "an empty corpus must yield None, not an empty catalog string");
    }

    // ── Tier-2: fetch_uow_snapshot prefers the object-wrapped context endpoint ──
    // The dedicated endpoint returns {"ok":true,"units_of_work":[...]}; the helper must parse the
    // OBJECT wrapper (parsing it as a bare array was the bug that silently emptied dev-state).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_uow_snapshot_parses_object_wrapper_from_context_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/development/context"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "units_of_work": [
                    { "story_id": "CAM-1", "stage": "development", "updated": "2026-06-24T00:00:00Z" }
                ]
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let snaps = super::fetch_uow_snapshot().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let snaps = snaps.expect("the context endpoint must yield Some");
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].story_id, "CAM-1");
        assert_eq!(snaps[0].stage, "development");
    }

    // When the context endpoint is unavailable (404), the helper falls back to the legacy bare-array
    // /api/uow endpoint.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_uow_snapshot_falls_back_to_legacy_uow_endpoint() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/development/context"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/uow"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "story_id": "CAM-9", "stage": "intake" }
            ])))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let snaps = super::fetch_uow_snapshot().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        let snaps = snaps.expect("legacy /api/uow fallback must yield Some");
        assert_eq!(snaps.len(), 1);
        assert_eq!(snaps[0].story_id, "CAM-9");
    }

    // ── Tier-2: fetch_project_context_sections GETs /api/projects/active/context ──
    // Asserts the four-tuple projection: name, scan section, selected-rules section, ruleset summary.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_project_context_sections_projects_all_four_fields() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/active/context"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "project_name": "agora-api",
                "scan_results_section": "Total findings: 3\n",
                "selected_rules_section": "Total selected: 2 rule(s)\n",
                "ruleset_summary": "SEC-1: all repos\n"
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let (name, scan, selected, ruleset) = super::fetch_project_context_sections().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(name, Some("agora-api".to_string()));
        assert_eq!(scan, Some("Total findings: 3\n".to_string()));
        assert_eq!(selected, Some("Total selected: 2 rule(s)\n".to_string()));
        assert_eq!(ruleset, Some("SEC-1: all repos\n".to_string()));
    }

    // When the context reports `ok: false` (no active project), all four fields degrade to None.
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_project_context_sections_all_none_when_not_ok() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/active/context"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "ok": false })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let tuple = super::fetch_project_context_sections().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(tuple, (None, None, None, None), "ok=false must degrade every section to None");
    }

    // Whitespace-only sections must be filtered to None (empty sections are not meaningful grounding).
    #[tokio::test]
    #[serial_test::serial(bff_env)]
    async fn fetch_project_context_sections_filters_whitespace_sections() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _env = bff_env_guard();
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/projects/active/context"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "project_name": "   ",
                "scan_results_section": "\n  \t",
                "selected_rules_section": "real\n"
            })))
            .mount(&server)
            .await;

        std::env::set_var("CAMERATA_BFF_URL", server.uri());
        let (name, scan, selected, ruleset) = super::fetch_project_context_sections().await;
        std::env::remove_var("CAMERATA_BFF_URL");

        assert_eq!(name, None, "whitespace-only project_name filters to None");
        assert_eq!(scan, None, "whitespace-only scan section filters to None");
        assert_eq!(selected, Some("real\n".to_string()), "non-blank section survives");
        assert_eq!(ruleset, None, "absent ruleset_summary is None");
    }

    // (chat_model_groups tests moved to camerata-ui-core::models — pure, now unit-tested with no
    // VirtualDom.)

    // ── chat_reply_text: backend errors are surfaced, not hidden ──────────────

    #[test]
    fn chat_reply_text_shows_backend_error_verbatim() {
        // A real backend failure (e.g. the `claude` CLI error) must be shown to
        // the user, not swallowed into a generic "no response" line.
        let out = chat_reply_text(Err("backend error 500: claude CLI exited 1: rate limited".into()));
        assert!(out.contains("chat failed"), "got: {out}");
        assert!(out.contains("claude CLI exited 1: rate limited"), "got: {out}");
    }

    #[test]
    fn chat_reply_text_returns_model_text_on_success() {
        let out = chat_reply_text(Ok(ChatResp { text: "hello".into(), backend: "cli".into() }));
        assert_eq!(out, "hello");
    }

    #[test]
    fn chat_reply_text_flags_empty_success() {
        let out = chat_reply_text(Ok(ChatResp { text: "   ".into(), backend: "cli".into() }));
        assert!(out.contains("empty response"), "got: {out}");
    }

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
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
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
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
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
        let prompt = unified_system_prompt(catalog, "No stories.\n", None, None, None, None, None, None);
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
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
        assert!(
            !prompt.contains("=== LAYER 2: GOVERNANCE RULES CATALOG"),
            "Unified prompt should omit LAYER 2 header when catalog is empty"
        );
    }

    #[test]
    fn unified_prompt_omits_rules_catalog_for_whitespace_only_input() {
        let prompt = unified_system_prompt("   \n\t  ", "No stories.\n", None, None, None, None, None, None);
        assert!(
            !prompt.contains("=== LAYER 2: GOVERNANCE RULES CATALOG"),
            "Unified prompt should omit LAYER 2 header for whitespace-only catalog"
        );
    }

    #[test]
    fn unified_prompt_contains_layer3_dev_state_header() {
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
        assert!(
            prompt.contains("=== LAYER 3: LIVE DEVELOPMENT STATE"),
            "Unified prompt missing LAYER 3 header"
        );
    }

    #[test]
    fn unified_prompt_layer3_header_names_active_project_when_present() {
        let prompt =
            unified_system_prompt("", "No stories.\n", None, None, None, None, None, Some("agora-api"));
        assert!(
            prompt.contains(
                "=== LAYER 3: LIVE DEVELOPMENT STATE (project: agora-api, refreshed this turn) ==="
            ),
            "Layer 3 header must name the active project when a name is supplied"
        );
    }

    #[test]
    fn unified_prompt_layer3_header_generic_when_no_project_name() {
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
        assert!(
            prompt.contains(
                "=== LAYER 3: LIVE DEVELOPMENT STATE (all tracked stories, refreshed this turn) ==="
            ),
            "Layer 3 header must fall back to the generic form when no project name is supplied"
        );
    }

    #[test]
    fn unified_prompt_layer3d_header_names_active_project_when_present() {
        let sel = "Total selected: 1 rule(s) across 1 repo(s)\nRules:\n  SEC-1 (all repos)\n";
        let prompt = unified_system_prompt(
            "",
            "No stories.\n",
            None,
            None,
            None,
            Some(sel),
            None,
            Some("agora-api"),
        );
        assert!(
            prompt.contains(
                "=== LAYER 3d: SELECTED RULES & OPTIONS (from agora-api onboarding draft) ==="
            ),
            "Layer 3d header must name the active project when a name is supplied"
        );
    }

    #[test]
    fn unified_prompt_layer3d_header_generic_when_no_project_name() {
        let sel = "Total selected: 1 rule(s) across 1 repo(s)\nRules:\n  SEC-1 (all repos)\n";
        let prompt =
            unified_system_prompt("", "No stories.\n", None, None, None, Some(sel), None, None);
        assert!(
            prompt.contains(
                "=== LAYER 3d: SELECTED RULES & OPTIONS (this project, from onboarding draft) ==="
            ),
            "Layer 3d header must fall back to the generic form when no project name is supplied"
        );
    }

    // ── honesty guardrail ─────────────────────────────────────────────────────

    #[test]
    fn unified_prompt_contains_not_covered_phrase() {
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
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
        let prompt = unified_system_prompt(catalog, uow, None, None, None, None, None, None);
        assert!(
            prompt.contains(UNIFIED_NOT_COVERED_PHRASE),
            "Unified prompt missing the not-covered phrase after adding catalog + uow"
        );
    }

    #[test]
    fn unified_prompt_not_covered_phrase_marked_critical() {
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
        assert!(
            prompt.contains("CRITICAL"),
            "Unified prompt should mark the not-covered guardrail as CRITICAL"
        );
    }

    /// The not-covered constraint must appear BEFORE the first layer header,
    /// so the model encounters the constraint before reading grounding data.
    #[test]
    fn unified_prompt_not_covered_phrase_appears_before_first_layer() {
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
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
        let prompt = unified_system_prompt(catalog, uow, None, None, None, None, None, None);
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
        let prompt = unified_system_prompt("", "No stories.\n", None, Some(&f), None, None, None, None);
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
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
        assert!(
            !prompt.contains("=== LAYER 4: FOCUSED FINDING"),
            "Prompt without finding must not include LAYER 4"
        );
    }

    #[test]
    fn unified_prompt_with_empty_finding_has_no_layer4() {
        let empty = FindingContext::default();
        let prompt = unified_system_prompt("", "No stories.\n", None, Some(&empty), None, None, None, None);
        assert!(
            !prompt.contains("=== LAYER 4: FOCUSED FINDING"),
            "Prompt with empty finding must not include LAYER 4"
        );
    }

    #[test]
    fn unified_prompt_with_finding_retains_not_covered_guardrail() {
        let f = make_finding();
        let prompt = unified_system_prompt("", "No stories.\n", None, Some(&f), None, None, None, None);
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
        let prompt = unified_system_prompt("", uow, None, Some(&f), None, None, None, None);
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

    // ── Layer 3b: pulled issues section ──────────────────────────────────────

    #[test]
    fn unified_prompt_layer3b_present_when_pulled_issues_supplied() {
        let issues = "- #10 [Epic, open]: Auth overhaul\n  - #11 [child, open]: Token refresh\n";
        let prompt = unified_system_prompt("", "No stories.\n", Some(issues), None, None, None, None, None);
        assert!(
            prompt.contains("=== LAYER 3b: PULLED ISSUES"),
            "Layer 3b header must appear when pulled_issues_section is Some"
        );
        assert!(
            prompt.contains(issues),
            "Pulled issues content must appear verbatim"
        );
    }

    #[test]
    fn unified_prompt_layer3b_absent_when_none() {
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
        assert!(
            !prompt.contains("=== LAYER 3b:"),
            "Layer 3b header must not appear when pulled_issues_section is None"
        );
    }

    #[test]
    fn unified_prompt_layer3b_absent_when_whitespace_only() {
        let prompt = unified_system_prompt("", "No stories.\n", Some("   \n "), None, None, None, None, None);
        assert!(
            !prompt.contains("=== LAYER 3b:"),
            "Layer 3b header must not appear for whitespace-only pulled_issues_section"
        );
    }

    #[test]
    fn layer3b_appears_after_layer3_and_before_layer4() {
        let f = make_finding();
        let uow = "- CAM-1: stage=development\n";
        let issues = "- #10 [Epic, open]: Auth\n";
        let prompt = unified_system_prompt("", uow, Some(issues), Some(&f), None, None, None, None);
        let layer3_pos = prompt
            .find("=== LAYER 3: LIVE DEVELOPMENT STATE")
            .expect("LAYER 3 not found");
        let layer3b_pos = prompt
            .find("=== LAYER 3b: PULLED ISSUES")
            .expect("LAYER 3b not found");
        let layer4_pos = prompt
            .find("=== LAYER 4: FOCUSED FINDING")
            .expect("LAYER 4 not found");
        assert!(
            layer3_pos < layer3b_pos,
            "LAYER 3 must precede LAYER 3b ({layer3_pos} < {layer3b_pos})"
        );
        assert!(
            layer3b_pos < layer4_pos,
            "LAYER 3b must precede LAYER 4 ({layer3b_pos} < {layer4_pos})"
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

    // ── Layer 3c: scan results section ───────────────────────────────────────

    #[test]
    fn unified_prompt_layer3c_present_when_scan_results_supplied() {
        let scan = "Total findings: 3\n  high: 2\n  medium: 1\n";
        let prompt = unified_system_prompt("", "No stories.\n", None, None, Some(scan), None, None, None);
        assert!(
            prompt.contains("=== LAYER 3c: ACTIVE PROJECT SCAN RESULTS"),
            "Layer 3c header must appear when scan_results_section is Some, got no header in prompt"
        );
        assert!(
            prompt.contains(scan),
            "Scan results content must appear verbatim"
        );
    }

    #[test]
    fn unified_prompt_layer3c_renders_none_marker_when_none() {
        // Absence is meaningful: Layer 3c ALWAYS renders, with an explicit NONE
        // marker when there are no scan results, so the model never invents findings.
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
        assert!(
            prompt.contains("=== LAYER 3c:"),
            "Layer 3c header must ALWAYS appear (absence is meaningful)"
        );
        assert!(
            prompt.contains("NONE — the active project has no scan results"),
            "Layer 3c must render an explicit NONE marker when scan results are absent"
        );
    }

    #[test]
    fn unified_prompt_layer3c_renders_none_marker_when_whitespace_only() {
        let prompt = unified_system_prompt("", "No stories.\n", None, None, Some("   \n "), None, None, None);
        assert!(
            prompt.contains("NONE — the active project has no scan results"),
            "whitespace-only scan results must render the explicit NONE marker"
        );
    }

    #[test]
    fn layer3c_appears_after_layer3b_and_before_layer4() {
        let f = make_finding();
        let uow = "- CAM-1: stage=development\n";
        let issues = "- #10 [Epic, open]: Auth\n";
        let scan = "Total findings: 1\n  high: 1\n";
        let prompt = unified_system_prompt("", uow, Some(issues), Some(&f), Some(scan), None, None, None);
        let layer3b_pos = prompt
            .find("=== LAYER 3b: PULLED ISSUES")
            .expect("LAYER 3b not found");
        let layer3c_pos = prompt
            .find("=== LAYER 3c: ACTIVE PROJECT SCAN RESULTS")
            .expect("LAYER 3c not found");
        let layer4_pos = prompt
            .find("=== LAYER 4: FOCUSED FINDING")
            .expect("LAYER 4 not found");
        assert!(
            layer3b_pos < layer3c_pos,
            "LAYER 3b must precede LAYER 3c ({layer3b_pos} < {layer3c_pos})"
        );
        assert!(
            layer3c_pos < layer4_pos,
            "LAYER 3c must precede LAYER 4 ({layer3c_pos} < {layer4_pos})"
        );
    }

    #[test]
    fn unified_prompt_preamble_mentions_scan_findings() {
        // The preamble must tell the model it can answer questions about scan findings.
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
        assert!(
            prompt.contains("scan findings") || prompt.contains("scan results"),
            "Preamble must mention scan findings so the model knows it can answer about them; \
             got preamble without mention"
        );
    }

    // ── Layer 3d: selected rules section ─────────────────────────────────────

    #[test]
    fn unified_prompt_layer3d_present_when_selected_rules_supplied() {
        let sel = "Total selected: 2 rule(s) across 1 repo(s)\nRules:\n  SEC-1 (all repos)\n";
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, Some(sel), None, None);
        assert!(
            prompt.contains("=== LAYER 3d: SELECTED RULES & OPTIONS"),
            "Layer 3d header must appear when selected_rules_section is Some"
        );
        assert!(
            prompt.contains("SEC-1"),
            "Selected rule id must appear verbatim in Layer 3d"
        );
    }

    #[test]
    fn unified_prompt_layer3d_renders_none_marker_when_none() {
        // Absence is meaningful: Layer 3d ALWAYS renders, with an explicit ZERO
        // marker, so the model never fabricates a selection count from Layer 2.
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
        assert!(
            prompt.contains("=== LAYER 3d:"),
            "Layer 3d header must ALWAYS appear (absence is meaningful)"
        );
        assert!(
            prompt.contains("ZERO rules selected"),
            "Layer 3d must render an explicit ZERO marker when no rules are selected"
        );
    }

    #[test]
    fn unified_prompt_layer3d_renders_none_marker_when_whitespace_only() {
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, Some("  \n "), None, None);
        assert!(
            prompt.contains("ZERO rules selected"),
            "whitespace-only selected rules must render the explicit ZERO marker"
        );
    }

    // ── Layer 3e: project committed ruleset section ──────────────────────────

    #[test]
    fn unified_prompt_layer3e_present_when_ruleset_supplied() {
        let ruleset = "SEC-NO-HARDCODED-SECRETS-1: all repos\nDOC-1: docs only\n";
        let prompt =
            unified_system_prompt("", "No stories.\n", None, None, None, None, Some(ruleset), None);
        assert!(
            prompt.contains(
                "=== LAYER 3e: PROJECT RULESET (committed rules governing this project) ==="
            ),
            "Layer 3e header must appear when project_ruleset_section is Some"
        );
        assert!(
            prompt.contains("SEC-NO-HARDCODED-SECRETS-1"),
            "committed ruleset content must appear verbatim in Layer 3e"
        );
    }

    #[test]
    fn unified_prompt_layer3e_renders_none_marker_when_none() {
        // Absence is meaningful: Layer 3e ALWAYS renders, with an explicit NONE
        // marker when there is no committed ruleset, so the model never infers the
        // governing rules from the Layer 2 catalog.
        let prompt = unified_system_prompt("", "No stories.\n", None, None, None, None, None, None);
        assert!(
            prompt.contains(
                "=== LAYER 3e: PROJECT RULESET (committed rules governing this project) ==="
            ),
            "Layer 3e header must ALWAYS appear (absence is meaningful)"
        );
        assert!(
            prompt.contains("NONE — no committed ruleset for this project yet"),
            "Layer 3e must render an explicit NONE marker when no committed ruleset is present"
        );
    }

    #[test]
    fn unified_prompt_layer3e_renders_none_marker_when_whitespace_only() {
        let prompt =
            unified_system_prompt("", "No stories.\n", None, None, None, None, Some("  \n "), None);
        assert!(
            prompt.contains("NONE — no committed ruleset for this project yet"),
            "whitespace-only committed ruleset must render the explicit NONE marker"
        );
    }

    #[test]
    fn layer3e_appears_after_layer3d() {
        let sel = "Total selected: 1 rule(s) across 1 repo(s)\nRules:\n  SEC-1 (all repos)\n";
        let ruleset = "SEC-1: all repos\n";
        let prompt = unified_system_prompt(
            "",
            "No stories.\n",
            None,
            None,
            None,
            Some(sel),
            Some(ruleset),
            None,
        );
        let layer3d_pos = prompt.find("=== LAYER 3d:").expect("LAYER 3d not found");
        let layer3e_pos = prompt.find("=== LAYER 3e:").expect("LAYER 3e not found");
        assert!(
            layer3d_pos < layer3e_pos,
            "LAYER 3d must precede LAYER 3e ({layer3d_pos} < {layer3e_pos})"
        );
    }

    #[test]
    fn unified_prompt_preamble_treats_changed_values_as_live_not_mistakes() {
        // Calibration guardrail: differing counts across turns are live updates,
        // not prior mistakes to apologize for; quote the exact count, no trends.
        let prompt = unified_system_prompt("CATALOG", "No stories.\n", None, None, None, None, None, None);
        assert!(
            prompt.contains("NOT a prior") && prompt.contains("WITHOUT apologizing"),
            "preamble must tell the model that a changed value is a live update, not a mistake to apologize for"
        );
        assert!(
            prompt.contains("Total selected: N") && prompt.contains("never estimate"),
            "preamble must tell the model to quote the exact selection count, not estimate"
        );
    }

    #[test]
    fn unified_prompt_preamble_forbids_inferring_selections_from_catalog() {
        // Anti-fabrication guardrail: the preamble must tell the model the Layer 2
        // catalog lists rules that EXIST, not rules the user selected.
        let prompt = unified_system_prompt("CATALOG", "No stories.\n", None, None, None, None, None, None);
        assert!(
            prompt.contains("NOT rules the user has selected"),
            "preamble must forbid deriving selected-rule counts from the Layer 2 catalog"
        );
    }

    #[test]
    fn development_context_response_parses_object_wrapper_not_bare_array() {
        // Locks the server contract: /api/development/context is an OBJECT
        // {"ok":true,"units_of_work":[...]} — parsing it as a bare array was the bug.
        let body = r#"{"ok":true,"units_of_work":[{"story_id":"CAM-1","stage":"development","updated":"2026-06-24T00:00:00Z"}]}"#;
        let parsed: DevelopmentContextResponse =
            serde_json::from_str(body).expect("object wrapper must deserialize");
        assert!(parsed.ok);
        assert_eq!(parsed.units_of_work.len(), 1);
        assert_eq!(parsed.units_of_work[0].story_id, "CAM-1");
        // And the old bare-array assumption must NOT parse the object (the original bug).
        assert!(
            serde_json::from_str::<Vec<UowSnapshot>>(body).is_err(),
            "object response must NOT parse as a bare array (regression guard)"
        );
    }

    #[test]
    fn layer3d_appears_after_layer3c() {
        let scan = "Total findings: 1\n";
        let sel = "Total selected: 1 rule(s) across 1 repo(s)\nRules:\n  SEC-1 (all repos)\n";
        let prompt =
            unified_system_prompt("", "No stories.\n", None, None, Some(scan), Some(sel), None, None);
        let layer3c_pos = prompt
            .find("=== LAYER 3c:")
            .expect("LAYER 3c not found");
        let layer3d_pos = prompt
            .find("=== LAYER 3d:")
            .expect("LAYER 3d not found");
        assert!(
            layer3c_pos < layer3d_pos,
            "LAYER 3c must precede LAYER 3d ({layer3c_pos} < {layer3d_pos})"
        );
    }

    // (ModelsResp::grouped provider-partitioning tests moved to camerata-ui-core::models.)
}
