//! Routine escalations: when a governed run is blocked and needs a human to decide,
//! the routine "stops here and escalates to a human reviewer." This module is the record
//! of that pause and its resolution.
//!
//! The loop (ADR/issue #43):
//!   1. A run is blocked (the gate denies an action it can't take unattended).
//!   2. An [`Escalation`] is raised: what it stopped for, why, and the routine's
//!      suggestions. The routine reads as **blocked — needs review** while it's open.
//!   3. A human gives a plain-language answer.
//!   4. [`translate_answer`] turns that answer into a precise resume directive for the
//!      routine (scaffolded today; AI-authored when Claude is connected — the prompt is
//!      a design fork flagged for review).
//!   5. The escalation resolves with the directive attached.
//!
//! NOTE: actually suspending a LIVE agent run mid-flight and resuming it with the
//! directive is the remaining run-engine wiring; today the directive is recorded on the
//! resolved escalation (the next run can consult it). The model, persistence, API, and
//! human-review UX are all real.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::routine::Routine;

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum EscalationStatus {
    Open,
    Resolved,
}

/// One turn in the human <-> lead-engineer review conversation. Chatting clarifies; it
/// never unblocks (only explicit authorization does).
#[derive(Clone, Serialize, Deserialize)]
pub struct EscalationMsg {
    /// "user" | "assistant"
    pub role: String,
    pub text: String,
    pub ts: String,
}

/// A blocked routine awaiting (or having received) human review.
#[derive(Clone, Serialize, Deserialize)]
pub struct Escalation {
    pub id: String,
    pub routine_id: String,
    /// Denormalized so the review panel reads standalone even if the routine is renamed.
    pub routine_name: String,
    /// Why it stopped — which rule / governance reason.
    pub reason: String,
    /// What decision the human actually needs to make.
    pub stopped_for: String,
    /// The routine's proposed options / recommendation.
    pub suggestions: Vec<String>,
    /// Any extra machine context (gate detail, etc.).
    #[serde(default)]
    pub raw_context: String,
    pub status: EscalationStatus,
    /// The human's plain-language decision (set on resolve).
    #[serde(default)]
    pub human_answer: Option<String>,
    /// The human answer translated into a resume directive for the routine.
    #[serde(default)]
    pub translated_directive: Option<String>,
    /// The STRUCTURED resume payload the AI-translation step produced (issue #43): the
    /// precise shape a routine resume consumes. `translated_directive` is its rendered
    /// human-readable text; this is the machine-usable record. `None` for escalations
    /// resolved before this field existed.
    #[serde(default)]
    pub resume_payload: Option<ResumePayload>,
    pub created: String,
    #[serde(default)]
    pub resolved: Option<String>,
    /// The human <-> lead-engineer review conversation. Chatting clarifies; only explicit
    /// authorization (resolve) unblocks the routine.
    #[serde(default)]
    pub conversation: Vec<EscalationMsg>,
}

/// Request to raise an escalation against a routine.
#[derive(Deserialize)]
pub struct RaiseEscalationReq {
    pub routine_id: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub stopped_for: String,
    #[serde(default)]
    pub suggestions: Vec<String>,
    #[serde(default)]
    pub raw_context: String,
}

/// Request body for resolving an escalation with a human answer.
#[derive(Deserialize)]
pub struct AnswerEscalationReq {
    pub answer: String,
}

/// The structured resume payload a routine needs to continue: the AI-translation step's
/// OUTPUT (issue #43). A human's plain-language answer is messy; a routine resume wants
/// something precise. This is that precise shape — the decision restated, the concrete
/// directive the agent should act on, and a confidence/needs-reescalation flag so an
/// ambiguous answer doesn't get applied blindly.
///
/// It is `Serialize`/`Deserialize` so the translator can return it as JSON (the model is
/// asked for exactly these fields) and so it travels over the API on the resolved
/// escalation.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResumePayload {
    /// The human's decision restated crisply (one line where possible).
    pub decision: String,
    /// The concrete directive the routine should act on to continue.
    pub directive: String,
    /// True when the answer was clear enough to apply; false means the routine should
    /// re-escalate rather than guess.
    pub confident: bool,
    /// How it was produced: `"claude"` (the lead-engineer agent authored it), `"echo"`
    /// (a test/offline driver), or `"scaffold"` (the deterministic fallback).
    pub authored_by: String,
}

impl ResumePayload {
    /// Render the payload as the human-readable resume directive stored on the escalation
    /// (back-compat with the existing `translated_directive` string the UI shows).
    pub fn to_directive_text(&self) -> String {
        let confidence = if self.confident {
            "Confident: apply and continue."
        } else {
            "LOW CONFIDENCE: the decision is ambiguous — re-escalate rather than guessing."
        };
        format!(
            "Decision: {decision}\n\nResume directive:\n{directive}\n\n{confidence}\n\n\
             [Translated by {by}.]",
            decision = self.decision,
            directive = self.directive,
            confidence = confidence,
            by = self.authored_by,
        )
    }
}

/// A driver that turns the translation prompt into a model completion. Abstracted so the
/// AI-translation step can be unit-tested with a FAKE/echo driver (no live model call),
/// while production uses the real [`crate::llm::Llm`] via [`LlmTranslator`].
///
/// Kept inside the server crate (no cross-crate trait surface added — ROUTE-1 safe).
#[async_trait::async_trait]
pub trait TranslationDriver: Send + Sync {
    /// Complete `prompt` (with `system` grounding) on `model`. Returns the raw model text
    /// (expected to be the JSON of a [`ResumePayload`], but the parser is lenient).
    async fn complete(&self, system: &str, prompt: &str, model: &str) -> anyhow::Result<String>;
}

/// Production driver: the real LLM seam. Thin wrapper so the handler can pass a
/// `&dyn TranslationDriver` and tests can swap in a fake.
pub struct LlmTranslator {
    pub llm: crate::llm::Llm,
    /// MULTI-REPO READ: the local clones of ALL the active project's repos. When non-empty,
    /// the translator runs WITH the first as cwd and every clone added read-only via
    /// `--add-dir` (CLI backend), so it can read the real code across all the project's repos
    /// while restating the human's decision — not just the inlined grounding digest.
    /// READ-ONLY and non-agentic — no write/exec tool is offered. Empty = digest-only (the
    /// prior behavior; e.g. the API backend or no local clone).
    pub repo_dirs: Vec<std::path::PathBuf>,
}

#[async_trait::async_trait]
impl TranslationDriver for LlmTranslator {
    async fn complete(&self, system: &str, prompt: &str, model: &str) -> anyhow::Result<String> {
        let mut req = crate::llm::LlmRequest::new(prompt)
            .with_system(system)
            .with_model(model);
        if !self.repo_dirs.is_empty() {
            req = req.with_repo_read_dirs(self.repo_dirs.iter().cloned());
        }
        let resp = self.llm.complete(req).await?;
        Ok(resp.text)
    }
}

/// System grounding for the translator agent: it restates a human decision as a precise,
/// rule-checked resume payload — it does NOT make the decision or take action.
pub fn translate_system_prompt() -> String {
    "You are Camerata's lead engineer translating a human's plain-language decision into a \
     PRECISE resume directive for a blocked, governed routine. You do NOT make the decision \
     or change it — you restate exactly what the human authorized as something the routine \
     can act on. Return ONLY a JSON object with these fields and nothing else: \
     {\"decision\": string (the human's decision, restated crisply), \
     \"directive\": string (the concrete action the routine should take to continue, under \
     its existing governance scope), \
     \"confident\": boolean (true only if the answer is clear enough to apply; false if it \
     is ambiguous or conflicts with the stated reason — in which case the routine should \
     re-escalate rather than guess)}. No prose, no markdown fences."
        .to_string()
}

/// Build the translation prompt: the escalation context + the human's raw answer.
pub fn translate_user_prompt(esc: &Escalation, answer: &str) -> String {
    let suggestions = if esc.suggestions.is_empty() {
        "(none offered)".to_string()
    } else {
        esc.suggestions
            .iter()
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "Routine: {name}\n\
         Why it stopped: {reason}\n\
         Decision needed: {stopped_for}\n\
         Options the routine proposed:\n{suggestions}\n\
         Additional context: {raw}\n\n\
         The human's plain-language decision:\n{answer}\n\n\
         Translate this into the JSON resume payload.",
        name = esc.routine_name,
        reason = esc.reason,
        stopped_for = esc.stopped_for,
        suggestions = suggestions,
        raw = if esc.raw_context.is_empty() {
            "(none)"
        } else {
            esc.raw_context.as_str()
        },
        answer = answer.trim(),
    )
}

/// Parse a translator's raw text into a [`ResumePayload`]. Lenient: accepts a bare JSON
/// object or one wrapped in ```json fences, and tolerates a missing `confident` (defaults
/// to true) so a terse model reply still yields a usable payload. Returns `None` when the
/// text isn't usable JSON — the caller then falls back to the deterministic scaffold.
pub fn parse_resume_payload(text: &str, authored_by: &str) -> Option<ResumePayload> {
    let trimmed = strip_code_fences(text.trim());
    // Find the first {...} span so leading/trailing prose doesn't defeat parsing.
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    if end < start {
        return None;
    }
    let json = &trimmed[start..=end];
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let decision = v["decision"].as_str()?.trim().to_string();
    let directive = v["directive"].as_str()?.trim().to_string();
    if decision.is_empty() || directive.is_empty() {
        return None;
    }
    // A missing `confident` defaults to true (the model gave a directive; treat as usable).
    let confident = v["confident"].as_bool().unwrap_or(true);
    Some(ResumePayload {
        decision,
        directive,
        confident,
        authored_by: authored_by.to_string(),
    })
}

/// Strip a leading/trailing ``` or ```json fence if present, so a fenced JSON reply parses.
fn strip_code_fences(s: &str) -> &str {
    let s = s.trim();
    let s = s
        .strip_prefix("```json")
        .or_else(|| s.strip_prefix("```"))
        .unwrap_or(s);
    s.strip_suffix("```").unwrap_or(s).trim()
}

/// The deterministic scaffold payload: used offline and as the fallback when the translator
/// is unreachable or returns unusable text. It restates the decision verbatim so the loop
/// always works and the human always sees what will be handed back.
pub fn scaffold_resume_payload(esc: &Escalation, answer: &str) -> ResumePayload {
    ResumePayload {
        decision: answer.trim().to_string(),
        directive: format!(
            "Apply the human decision above to routine \"{name}\" and continue under its \
             existing governance scope. Stopped for: {stopped_for}. If the decision is \
             ambiguous or conflicts with a rule, stop and escalate again rather than \
             guessing.",
            name = esc.routine_name,
            stopped_for = esc.stopped_for,
        ),
        confident: !answer.trim().is_empty(),
        authored_by: "scaffold".to_string(),
    }
}

/// Turn a human's plain-language answer into a precise resume directive for the routine —
/// the AI-translation step (issue #43), routed through a [`TranslationDriver`] so it's
/// unit-testable with a fake/echo driver and runs on the model the caller selects.
///
/// On any failure (driver error, empty/unparseable reply) it falls back to the deterministic
/// [`scaffold_resume_payload`], so resolving an escalation NEVER dead-ends. Returns the
/// structured [`ResumePayload`]; call [`ResumePayload::to_directive_text`] for the string the
/// UI shows.
pub async fn translate_answer_ai(
    driver: &dyn TranslationDriver,
    esc: &Escalation,
    answer: &str,
    model: &str,
    grounding: Option<&str>,
) -> ResumePayload {
    // GROUNDING (the invariant): even the translation step is grounded in the active
    // project's repo + rules, so the restated directive is checked against the ACTUAL stack.
    let system = match grounding {
        Some(g) if !g.trim().is_empty() => format!("{}\n\n{g}", translate_system_prompt()),
        _ => translate_system_prompt(),
    };
    let user = translate_user_prompt(esc, answer);
    match driver.complete(&system, &user, model).await {
        Ok(text) => parse_resume_payload(&text, "claude")
            .unwrap_or_else(|| scaffold_resume_payload(esc, answer)),
        Err(_) => scaffold_resume_payload(esc, answer),
    }
}

/// Deterministic translation (no model call): restate the decision in the structured form a
/// routine resume expects, so the loop works offline and synchronously. Returns
/// `(directive_text, authored_by)`. The [`EscalationStore::resolve`] path uses this; the
/// HTTP handler uses [`translate_answer_ai`] for the AI-authored version.
pub fn translate_answer(esc: &Escalation, answer: &str) -> (String, String) {
    let payload = scaffold_resume_payload(esc, answer);
    (payload.to_directive_text(), payload.authored_by)
}

/// System grounding for the lead-engineer review agent. It must HELP the human understand
/// and decide, and must NOT act — only the human's explicit authorization (a separate
/// control) unblocks the routine.
pub fn chat_system_prompt(esc: &Escalation) -> String {
    let suggestions = if esc.suggestions.is_empty() {
        "(none offered)".to_string()
    } else {
        esc.suggestions
            .iter()
            .map(|s| format!("- {s}"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "You are Camerata's lead engineer. An autonomous, governed routine has STOPPED and \
         escalated to a human for a decision. Your job is to help the human UNDERSTAND the \
         situation and decide: explain trade-offs, answer clarifying questions, and lay out \
         the pros and cons of each option. You must NOT take any action or unblock anything \
         yourself; only the human's explicit authorization (a separate control they press) \
         unblocks the routine. Be concise and concrete. When the human states a decision, \
         restate it crisply and remind them to use the Authorize control to apply it.\n\n\
         Routine: {name}\n\
         Why it stopped: {reason}\n\
         Decision needed: {stopped_for}\n\
         Options the routine proposed:\n{suggestions}\n\
         Additional context: {raw}",
        name = esc.routine_name,
        reason = esc.reason,
        stopped_for = esc.stopped_for,
        raw = if esc.raw_context.is_empty() {
            "(none)"
        } else {
            esc.raw_context.as_str()
        },
    )
}

/// Fold the prior conversation + the new user message into one prompt for the single-shot
/// completion backend (which has no native multi-turn memory).
pub fn chat_user_prompt(esc: &Escalation, new_message: &str) -> String {
    let mut s = String::new();
    if !esc.conversation.is_empty() {
        s.push_str("Conversation so far:\n");
        for m in &esc.conversation {
            s.push_str(&format!("{}: {}\n", m.role, m.text));
        }
        s.push('\n');
    }
    s.push_str(&format!(
        "user: {new_message}\n\nRespond as the lead engineer to the latest user message."
    ));
    s
}

/// Build the generic escalation a governed run raises when it is blocked (gate denials)
/// during an unattended/scheduled run. Richer, rule-level detail is a follow-up (it needs
/// `run_now` to surface the denied rule ids); this is the honest first cut.
pub fn blocked_run_escalation_req(routine: &Routine, denies: usize) -> RaiseEscalationReq {
    let denied_rules: Vec<String> = routine
        .last_run
        .as_ref()
        .map(|s| s.denied_rules.clone())
        .unwrap_or_default();
    let rules_clause = if denied_rules.is_empty() {
        String::new()
    } else {
        format!(" (rule(s): {})", denied_rules.join(", "))
    };
    let mut raw_context = format!("scope: {}", routine.scope);
    if !denied_rules.is_empty() {
        raw_context.push_str(&format!("\ndenied rules: {}", denied_rules.join(", ")));
    }
    RaiseEscalationReq {
        routine_id: routine.id.clone(),
        reason: format!(
            "The governed run was blocked by {denies} gate denial(s){rules_clause} — an action \
             the routine can't take unattended and that needs a human decision."
        ),
        stopped_for: "Whether to proceed past the blocked action(s), adjust the routine, \
                      or cancel this run."
            .to_string(),
        suggestions: vec![
            "Approve and proceed past the blocked action".to_string(),
            "Adjust the routine's scope or prompt, then re-run".to_string(),
            "Cancel this run".to_string(),
        ],
        raw_context,
    }
}

/// Raise a blocked-run escalation for `routine` if its last run had gate denials (the
/// signal that it stopped and needs a human). Deduped, so a routine has at most one open
/// review. Called by both the interactive run handler and the auto-fire scheduler.
pub fn raise_if_blocked(store: &EscalationStore, routine: &Routine) {
    let denies = routine.last_run.as_ref().map(|s| s.denies).unwrap_or(0);
    if denies > 0 {
        let req = blocked_run_escalation_req(routine, denies);
        store.raise_deduped(req, &routine.name);
    }
}

// ── store ───────────────────────────────────────────────────────────────────────

/// Escalation store. In-memory by default; [`at`] persists to
/// `<data_dir>/camerata/escalations.json`. `Clone` is a shallow Arc handle for
/// [`crate::AppState`]. Mirrors `RoutineStore`'s persistence shape.
#[derive(Clone, Default)]
pub struct EscalationStore {
    items: Arc<Mutex<Vec<Escalation>>>,
    counter: Arc<AtomicUsize>,
    path: Option<Arc<PathBuf>>,
}

impl EscalationStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn at(path: PathBuf) -> Self {
        let items: Vec<Escalation> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        let max = items
            .iter()
            .filter_map(|e| e.id.strip_prefix("esc-"))
            .filter_map(|n| n.parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        Self {
            items: Arc::new(Mutex::new(items)),
            counter: Arc::new(AtomicUsize::new(max)),
            path: Some(Arc::new(path)),
        }
    }

    fn now_rfc3339() -> String {
        chrono::Utc::now().to_rfc3339()
    }

    fn flush(&self) {
        let Some(p) = &self.path else { return };
        let Ok(items) = self.items.lock() else { return };
        if let Ok(s) = serde_json::to_string(&*items) {
            let _ = std::fs::write(p.as_ref(), s);
        }
    }

    pub fn list(&self) -> Vec<Escalation> {
        self.items.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn list_open(&self) -> Vec<Escalation> {
        self.items
            .lock()
            .map(|g| {
                g.iter()
                    .filter(|e| e.status == EscalationStatus::Open)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The open escalation for a routine, if one exists (a routine has at most one open
    /// review at a time — raising is deduped on this).
    pub fn open_for_routine(&self, routine_id: &str) -> Option<Escalation> {
        self.items
            .lock()
            .ok()?
            .iter()
            .find(|e| e.routine_id == routine_id && e.status == EscalationStatus::Open)
            .cloned()
    }

    /// One escalation by id.
    pub fn get(&self, id: &str) -> Option<Escalation> {
        self.items.lock().ok()?.iter().find(|e| e.id == id).cloned()
    }

    /// Append a user message + the lead-engineer's reply to an escalation's conversation.
    /// Chatting never resolves the escalation (only explicit authorization does), so even a
    /// resolved escalation can still be discussed as a read-back. Returns the updated record.
    pub fn append_turn(
        &self,
        id: &str,
        user_text: &str,
        assistant_text: &str,
    ) -> Option<Escalation> {
        let mut guard = self.items.lock().ok()?;
        let e = guard.iter_mut().find(|e| e.id == id)?;
        let ts = Self::now_rfc3339();
        e.conversation.push(EscalationMsg {
            role: "user".to_string(),
            text: user_text.to_string(),
            ts: ts.clone(),
        });
        e.conversation.push(EscalationMsg {
            role: "assistant".to_string(),
            text: assistant_text.to_string(),
            ts,
        });
        let updated = e.clone();
        drop(guard);
        self.flush();
        Some(updated)
    }

    /// Raise an escalation. `routine_name` is denormalized in for standalone display.
    pub fn raise(&self, req: RaiseEscalationReq, routine_name: &str) -> Escalation {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let esc = Escalation {
            id: format!("esc-{n}"),
            routine_id: req.routine_id,
            routine_name: routine_name.to_string(),
            reason: req.reason,
            stopped_for: req.stopped_for,
            suggestions: req.suggestions,
            raw_context: req.raw_context,
            status: EscalationStatus::Open,
            human_answer: None,
            translated_directive: None,
            resume_payload: None,
            created: Self::now_rfc3339(),
            resolved: None,
            conversation: Vec::new(),
        };
        if let Ok(mut guard) = self.items.lock() {
            guard.push(esc.clone());
        }
        self.flush();
        esc
    }

    /// Raise only if the routine has no open escalation already; returns the new (or
    /// existing open) escalation. Used by the run path so a blocked routine doesn't pile
    /// up duplicate reviews.
    pub fn raise_deduped(&self, req: RaiseEscalationReq, routine_name: &str) -> Escalation {
        if let Some(existing) = self.open_for_routine(&req.routine_id) {
            return existing;
        }
        self.raise(req, routine_name)
    }

    /// Resolve an escalation with the human answer, translating it with the DETERMINISTIC
    /// scaffold (no model call). Returns the updated escalation, or `None` if the id is
    /// unknown or already resolved. The HTTP handler prefers [`resolve_with_payload`] so the
    /// AI-authored translation is stored; this stays for the offline/synchronous path.
    pub fn resolve(&self, id: &str, answer: &str) -> Option<Escalation> {
        let payload = {
            let guard = self.items.lock().ok()?;
            let e = guard
                .iter()
                .find(|e| e.id == id && e.status == EscalationStatus::Open)?;
            scaffold_resume_payload(e, answer)
        };
        self.resolve_with_payload(id, answer, &payload)
    }

    /// Resolve an escalation with the human answer + an already-translated [`ResumePayload`]
    /// (e.g. the AI-authored one from [`translate_answer_ai`]). Stores both the rendered
    /// directive text (for the UI) and the structured payload. Returns the updated
    /// escalation, or `None` if the id is unknown or already resolved.
    pub fn resolve_with_payload(
        &self,
        id: &str,
        answer: &str,
        payload: &ResumePayload,
    ) -> Option<Escalation> {
        let mut guard = self.items.lock().ok()?;
        let e = guard
            .iter_mut()
            .find(|e| e.id == id && e.status == EscalationStatus::Open)?;
        e.human_answer = Some(answer.to_string());
        e.translated_directive = Some(payload.to_directive_text());
        e.resume_payload = Some(payload.clone());
        e.status = EscalationStatus::Resolved;
        e.resolved = Some(Self::now_rfc3339());
        let updated = e.clone();
        drop(guard);
        self.flush();
        Some(updated)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(routine_id: &str) -> RaiseEscalationReq {
        RaiseEscalationReq {
            routine_id: routine_id.to_string(),
            reason: "blocked by an architectural-decision rule".to_string(),
            stopped_for: "which storage backend to use".to_string(),
            suggestions: vec!["Postgres".to_string(), "SQLite".to_string()],
            raw_context: String::new(),
        }
    }

    #[test]
    fn raise_dedupes_per_routine_until_resolved() {
        let store = EscalationStore::new();
        let a = store.raise_deduped(req("rt-1"), "Nightly");
        // Second raise while open returns the SAME open escalation.
        let b = store.raise_deduped(req("rt-1"), "Nightly");
        assert_eq!(a.id, b.id);
        assert_eq!(store.list_open().len(), 1);
        assert!(store.open_for_routine("rt-1").is_some());

        // A different routine gets its own.
        store.raise_deduped(req("rt-2"), "Auditor");
        assert_eq!(store.list_open().len(), 2);
    }

    #[test]
    fn resolve_translates_and_closes() {
        let store = EscalationStore::new();
        let e = store.raise_deduped(req("rt-1"), "Nightly");

        let resolved = store.resolve(&e.id, "Use Postgres").expect("resolved");
        assert_eq!(resolved.status, EscalationStatus::Resolved);
        assert_eq!(resolved.human_answer.as_deref(), Some("Use Postgres"));
        let directive = resolved.translated_directive.expect("directive");
        assert!(
            directive.contains("Use Postgres"),
            "answer carried into directive"
        );
        assert!(directive.contains("Nightly"), "routine name in directive");

        // Now no open escalation for the routine; resolving again is a no-op None.
        assert!(store.open_for_routine("rt-1").is_none());
        assert!(store.resolve(&e.id, "again").is_none());

        // After resolving, a fresh block can raise a NEW escalation.
        let f = store.raise_deduped(req("rt-1"), "Nightly");
        assert_ne!(f.id, e.id);
    }

    #[test]
    fn blocked_run_req_is_well_formed() {
        let routine = crate::routine::RoutineStore::seeded().list()[0].clone();
        let r = blocked_run_escalation_req(&routine, 2);
        assert_eq!(r.routine_id, routine.id);
        assert!(r.reason.contains('2'));
        assert_eq!(r.suggestions.len(), 3);
    }

    // ── AI-translation step (fake/echo drivers; never a live model call) ──────────────

    /// A driver that returns whatever JSON it was constructed with — so the parser +
    /// shaper can be tested deterministically without touching a real model.
    struct CannedDriver(String);

    #[async_trait::async_trait]
    impl TranslationDriver for CannedDriver {
        async fn complete(
            &self,
            _system: &str,
            _prompt: &str,
            _model: &str,
        ) -> anyhow::Result<String> {
            Ok(self.0.clone())
        }
    }

    /// A driver that ECHOES the prompt back (NOT valid JSON), proving the fallback path:
    /// an unusable reply yields the deterministic scaffold rather than dead-ending.
    struct EchoDriver;

    #[async_trait::async_trait]
    impl TranslationDriver for EchoDriver {
        async fn complete(
            &self,
            _system: &str,
            prompt: &str,
            _model: &str,
        ) -> anyhow::Result<String> {
            Ok(format!("Here is the prompt I received:\n{prompt}"))
        }
    }

    /// A driver that always errors, proving the error branch falls back to the scaffold.
    struct FailingDriver;

    #[async_trait::async_trait]
    impl TranslationDriver for FailingDriver {
        async fn complete(
            &self,
            _system: &str,
            _prompt: &str,
            _model: &str,
        ) -> anyhow::Result<String> {
            anyhow::bail!("model unreachable")
        }
    }

    fn open_escalation() -> Escalation {
        let store = EscalationStore::new();
        store.raise(req("rt-1"), "Nightly")
    }

    #[tokio::test]
    async fn translate_shapes_valid_json_into_payload() {
        let esc = open_escalation();
        let json = r#"{"decision":"Use Postgres","directive":"Provision a Postgres backend and continue.","confident":true}"#;
        let driver = CannedDriver(json.to_string());
        let payload =
            translate_answer_ai(&driver, &esc, "go with postgres", "claude-sonnet-4-6", None).await;
        assert_eq!(payload.decision, "Use Postgres");
        assert!(payload.directive.contains("Postgres"));
        assert!(payload.confident);
        assert_eq!(payload.authored_by, "claude");
        // The rendered directive text carries the decision (what the UI shows).
        assert!(payload.to_directive_text().contains("Use Postgres"));
    }

    #[tokio::test]
    async fn translate_tolerates_code_fences_and_surrounding_prose() {
        let esc = open_escalation();
        let fenced = "Sure, here it is:\n```json\n{\"decision\":\"Cancel the run\",\"directive\":\"Abort and report.\",\"confident\":false}\n```\nThanks!";
        let driver = CannedDriver(fenced.to_string());
        let payload = translate_answer_ai(&driver, &esc, "cancel it", "m", None).await;
        assert_eq!(payload.decision, "Cancel the run");
        assert!(!payload.confident, "confident:false carried through");
        assert_eq!(payload.authored_by, "claude");
    }

    #[tokio::test]
    async fn translate_falls_back_to_scaffold_on_unparseable_reply() {
        let esc = open_escalation();
        // Echo driver returns prose, not JSON -> the scaffold takes over (never dead-ends).
        let payload = translate_answer_ai(&EchoDriver, &esc, "use option B", "m", None).await;
        assert_eq!(payload.authored_by, "scaffold");
        // The scaffold restates the human's raw answer as the decision.
        assert_eq!(payload.decision, "use option B");
        assert!(payload.confident, "non-empty answer -> confident scaffold");
        assert!(
            payload.directive.contains("Nightly"),
            "routine name in directive"
        );
    }

    #[tokio::test]
    async fn translate_falls_back_to_scaffold_on_driver_error() {
        let esc = open_escalation();
        let payload = translate_answer_ai(&FailingDriver, &esc, "approve", "m", None).await;
        assert_eq!(payload.authored_by, "scaffold");
        assert_eq!(payload.decision, "approve");
    }

    #[test]
    fn parse_rejects_empty_or_missing_required_fields() {
        // Missing directive.
        assert!(parse_resume_payload(r#"{"decision":"x"}"#, "claude").is_none());
        // Empty decision.
        assert!(parse_resume_payload(r#"{"decision":"  ","directive":"y"}"#, "claude").is_none());
        // Not JSON at all.
        assert!(parse_resume_payload("just some words", "claude").is_none());
        // Missing `confident` defaults to true (terse but usable).
        let p = parse_resume_payload(r#"{"decision":"x","directive":"y"}"#, "claude").unwrap();
        assert!(p.confident);
    }

    #[tokio::test]
    async fn resolve_with_payload_stores_structured_and_rendered_forms() {
        let store = EscalationStore::new();
        let e = store.raise(req("rt-1"), "Nightly");
        let json = r#"{"decision":"Use SQLite","directive":"Switch the store to SQLite and continue.","confident":true}"#;
        let payload =
            translate_answer_ai(&CannedDriver(json.to_string()), &e, "sqlite please", "m", None).await;

        let resolved = store
            .resolve_with_payload(&e.id, "sqlite please", &payload)
            .expect("resolved");
        assert_eq!(resolved.status, EscalationStatus::Resolved);
        assert_eq!(resolved.human_answer.as_deref(), Some("sqlite please"));
        // Structured payload is recorded for the resumed run to consult.
        let stored = resolved.resume_payload.expect("payload recorded");
        assert_eq!(stored.decision, "Use SQLite");
        assert_eq!(stored.authored_by, "claude");
        // Rendered directive carries the decision (what the UI shows).
        assert!(resolved
            .translated_directive
            .unwrap()
            .contains("Use SQLite"));

        // Already resolved -> resolving again is a no-op None.
        assert!(store.resolve_with_payload(&e.id, "x", &payload).is_none());
    }
}
