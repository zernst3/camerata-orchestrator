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

/// Turn a human's plain-language answer into a precise resume directive for the routine.
///
/// This is the deterministic scaffold: it restates the decision in the structured form a
/// routine resume expects, so the loop works offline and the human always sees what will
/// be handed back. When Claude is connected, the lead-engineer agent authors this for
/// real (the prompt + the translator agent's scope is the design fork flagged in #43).
/// Returns `(directive, authored_by)` where `authored_by` is `"scaffold"` today.
pub fn translate_answer(esc: &Escalation, answer: &str) -> (String, String) {
    let directive = format!(
        "Resume directive for routine \"{name}\" (escalation {id}).\n\n\
         Human decision:\n{answer}\n\n\
         This resolves the block:\n- Stopped for: {stopped_for}\n- Reason: {reason}\n\n\
         Apply the decision above and continue the routine under its existing governance \
         scope. If the decision is ambiguous or conflicts with a rule, stop and escalate \
         again rather than guessing.\n\n\
         [Translated by scaffold — connect Claude so the lead-engineer agent restates the \
         decision as a precise, rule-checked resume directive.]",
        name = esc.routine_name,
        id = esc.id,
        answer = answer.trim(),
        stopped_for = esc.stopped_for,
        reason = esc.reason,
    );
    (directive, "scaffold".to_string())
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
        raw = if esc.raw_context.is_empty() { "(none)" } else { esc.raw_context.as_str() },
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
    RaiseEscalationReq {
        routine_id: routine.id.clone(),
        reason: format!(
            "The governed run was blocked by {denies} gate denial(s) — an action the \
             routine can't take unattended and that needs a human decision."
        ),
        stopped_for: "Whether to proceed past the blocked action(s), adjust the routine, \
                      or cancel this run."
            .to_string(),
        suggestions: vec![
            "Approve and proceed past the blocked action".to_string(),
            "Adjust the routine's scope or prompt, then re-run".to_string(),
            "Cancel this run".to_string(),
        ],
        raw_context: format!("scope: {}", routine.scope),
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
            .map(|g| g.iter().filter(|e| e.status == EscalationStatus::Open).cloned().collect())
            .unwrap_or_default()
    }

    /// The open escalation for a routine, if one exists (a routine has at most one open
    /// review at a time — raising is deduped on this).
    pub fn open_for_routine(&self, routine_id: &str) -> Option<Escalation> {
        self.items.lock().ok()?.iter().find(|e| {
            e.routine_id == routine_id && e.status == EscalationStatus::Open
        }).cloned()
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

    /// Resolve an escalation with the human answer + its translated directive. Returns the
    /// updated escalation, or `None` if the id is unknown or already resolved.
    pub fn resolve(&self, id: &str, answer: &str) -> Option<Escalation> {
        let mut guard = self.items.lock().ok()?;
        let e = guard
            .iter_mut()
            .find(|e| e.id == id && e.status == EscalationStatus::Open)?;
        let (directive, _authored_by) = translate_answer(e, answer);
        e.human_answer = Some(answer.to_string());
        e.translated_directive = Some(directive);
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
        assert!(directive.contains("Use Postgres"), "answer carried into directive");
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
}
