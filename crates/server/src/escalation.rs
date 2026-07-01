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
//!
//! Domain types + pure functions + the [`TranslationDriver`] seam live in
//! `camerata_app_core::escalation`; this module owns the `EscalationStore`
//! (Arc<Mutex> + fs persistence), `LlmTranslator` (the production driver), and
//! `raise_if_blocked`.

pub use camerata_app_core::escalation::{
    AnswerEscalationReq, Escalation, EscalationAction, EscalationMsg, EscalationStatus,
    RaiseEscalationReq, ResumePayload, SubjectKind, TranslationDriver, blocked_run_escalation_req,
    chat_system_prompt, chat_user_prompt, parse_resume_payload, scaffold_resume_payload,
    translate_answer, translate_answer_ai, translate_system_prompt, translate_user_prompt,
};

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::routine::Routine;

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

/// Raise a blocked-run escalation for `routine` if its last run had gate denials (the
/// signal that it stopped and needs a human). Deduped, so a routine has at most one open
/// review. Called by both the interactive run handler and the auto-fire scheduler. Returns the
/// escalation id when one is raised (or the existing open one), so the caller can link it to the
/// routine's run-history entry; `None` when the run was clean.
pub fn raise_if_blocked(store: &EscalationStore, routine: &Routine) -> Option<String> {
    let denies = routine.last_run.as_ref().map(|s| s.denies).unwrap_or(0);
    if denies > 0 {
        let req = blocked_run_escalation_req(routine, denies);
        let esc = store.raise_deduped(req, &routine.name);
        Some(esc.id)
    } else {
        None
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

    /// The open escalation for a specific SUBJECT (kind + id), if any. Dedup keys off this, so a
    /// subject has at most one open review at a time.
    pub fn open_for_subject(&self, kind: SubjectKind, id: &str) -> Option<Escalation> {
        self.items
            .lock()
            .ok()?
            .iter()
            .find(|e| e.subject_kind == kind && e.routine_id == id && e.status == EscalationStatus::Open)
            .cloned()
    }

    /// The open escalation for a routine, if one exists.
    pub fn open_for_routine(&self, routine_id: &str) -> Option<Escalation> {
        self.open_for_subject(SubjectKind::Routine, routine_id)
    }

    /// The open escalation for a UoW (its `story_id`), if one exists.
    pub fn open_for_uow(&self, story_id: &str) -> Option<Escalation> {
        self.open_for_subject(SubjectKind::Uow, story_id)
    }

    /// All open UoW escalations for a story (normally 0 or 1; a Vec for the NEEDS YOU surface).
    pub fn list_open_for_uow(&self, story_id: &str) -> Vec<Escalation> {
        self.list_open()
            .into_iter()
            .filter(|e| e.subject_kind == SubjectKind::Uow && e.routine_id == story_id)
            .collect()
    }

    /// All open UoW escalations across every story (drives the global NEEDS YOU count).
    pub fn list_open_uow(&self) -> Vec<Escalation> {
        self.list_open()
            .into_iter()
            .filter(|e| e.subject_kind == SubjectKind::Uow)
            .collect()
    }

    /// One escalation by id.
    pub fn get(&self, id: &str) -> Option<Escalation> {
        self.items.lock().ok()?.iter().find(|e| e.id == id).cloned()
    }

    /// Link a UoW escalation to the checkpoint a resume continues from. Set once at pause time
    /// (idempotent). Returns the updated record (or `None` for an unknown id).
    pub fn set_checkpoint(&self, id: &str, checkpoint_id: &str) -> Option<Escalation> {
        let mut guard = self.items.lock().ok()?;
        let e = guard.iter_mut().find(|e| e.id == id)?;
        e.checkpoint_id = Some(checkpoint_id.to_string());
        let updated = e.clone();
        drop(guard);
        self.flush();
        Some(updated)
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
            subject_kind: req.subject_kind,
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
            checkpoint_id: req.checkpoint_id,
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
        if let Some(existing) = self.open_for_subject(req.subject_kind, &req.routine_id) {
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
            subject_kind: SubjectKind::Routine,
            checkpoint_id: None,
            routine_id: routine_id.to_string(),
            reason: "blocked by an architectural-decision rule".to_string(),
            stopped_for: "which storage backend to use".to_string(),
            suggestions: vec!["Postgres".to_string(), "SQLite".to_string()],
            raw_context: String::new(),
        }
    }

    fn uow_req(story_id: &str, ckpt: &str) -> RaiseEscalationReq {
        RaiseEscalationReq {
            subject_kind: SubjectKind::Uow,
            checkpoint_id: Some(ckpt.to_string()),
            routine_id: story_id.to_string(),
            reason: "test-tamper".to_string(),
            stopped_for: "approve the test edit?".to_string(),
            suggestions: vec![],
            raw_context: String::new(),
        }
    }

    #[test]
    fn uow_escalations_are_scoped_separately_from_routines() {
        let store = EscalationStore::new();
        // A routine escalation and a UoW escalation that share the same id string do NOT collide;
        // subject_kind disambiguates.
        let routine_esc = store.raise(req("x"), "Nightly");
        let uow_esc = store.raise_deduped(uow_req("x", "ckpt-1"), "Story x");
        assert_ne!(routine_esc.id, uow_esc.id, "routine + uow with same id are distinct");
        assert_eq!(uow_esc.subject_kind, SubjectKind::Uow);
        assert_eq!(uow_esc.checkpoint_id.as_deref(), Some("ckpt-1"));
        // open_for_* is subject-scoped.
        assert_eq!(store.open_for_routine("x").map(|e| e.id.clone()), Some(routine_esc.id));
        assert_eq!(store.open_for_uow("x").map(|e| e.id.clone()), Some(uow_esc.id.clone()));
        assert_eq!(store.list_open_for_uow("x").len(), 1);
        assert_eq!(store.list_open_uow().len(), 1);
        // Dedup per subject: raising the same UoW again returns the existing open one.
        let again = store.raise_deduped(uow_req("x", "ckpt-2"), "Story x");
        assert_eq!(again.id, uow_esc.id, "uow dedup returns the open one");
        assert_eq!(store.list_open_uow().len(), 1);
    }

    #[test]
    fn set_checkpoint_links_escalation_to_its_checkpoint() {
        // The pause flow raises the escalation with no checkpoint yet, then links it once the
        // checkpoint is created (chicken/egg: the checkpoint records the escalation id too).
        let store = EscalationStore::new();
        let esc = store.raise(
            RaiseEscalationReq {
                subject_kind: SubjectKind::Uow,
                checkpoint_id: None,
                routine_id: "s#1".to_string(),
                reason: "test-tamper".to_string(),
                stopped_for: String::new(),
                suggestions: vec![],
                raw_context: String::new(),
            },
            "Story",
        );
        assert!(esc.checkpoint_id.is_none());
        let linked = store.set_checkpoint(&esc.id, "ckpt-7").unwrap();
        assert_eq!(linked.checkpoint_id.as_deref(), Some("ckpt-7"));
        assert_eq!(store.get(&esc.id).unwrap().checkpoint_id.as_deref(), Some("ckpt-7"));
        assert!(store.set_checkpoint("nope", "ckpt-7").is_none(), "unknown id -> None");
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

    // ── AI-translation step (fake/echo drivers via re-export; never a live model call) ─

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
