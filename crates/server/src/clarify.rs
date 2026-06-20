//! Phase 4: the clarify-bridge (in-process / native).
//!
//! When a story needs a product decision, the architect posts a clarifying question
//! to an addressee (the per-question pick from the cockpit-UX ADR), and the answer
//! comes back and unblocks the story. This module models that round-trip in-process:
//! a clarification is created (open), then answered.
//!
//! Honesty note: this is the native, in-process round-trip. The Phase-5 step is
//! wiring the POST through the `WorkItemProvider` so the question is written as a real
//! comment on a real tracker item (GitHub/ADO/Jira) with the addressee @-mentioned,
//! and the answer is polled back. The model here is what the cockpit composer drives;
//! the live-transport write-back is deferred to the provider work.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// The lifecycle state of a clarification: posted-and-waiting vs. answered.
///
/// This is the persisted asked -> answered transition (issue #22 gap (c)).
/// Previously the state was implicit in `answer.is_some()`; making it an explicit,
/// serialized enum lets the cockpit render an unambiguous status badge and lets
/// any future transport (the live tracker round-trip) record the transition
/// without re-deriving it from a nullable field.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClarifyState {
    /// The question has been posted and is awaiting an answer.
    Asked,
    /// An answer has been recorded.
    Answered,
}

/// One clarifying question on a story: who it is addressed to, and the answer once
/// it comes back.
#[derive(Clone, Serialize)]
pub struct Clarification {
    pub id: String,
    pub story_id: String,
    pub question: String,
    /// Who the question is addressed to (the per-question pick: a teammate, "you",
    /// or a free-typed handle). Not a standing "PO" role.
    pub addressee: String,
    pub answer: Option<String>,
    pub answered_by: Option<String>,
    /// The explicit, persisted lifecycle state. Always consistent with `answer`:
    /// `Answered` iff `answer.is_some()`. Serialized so clients can branch on it
    /// directly rather than re-deriving from the nullable `answer`.
    pub state: ClarifyState,
}

impl Clarification {
    pub fn is_open(&self) -> bool {
        self.answer.is_none()
    }

    /// The lifecycle state, derived from whether an answer is present. This is the
    /// single source of truth the stored `state` field mirrors.
    pub fn state(&self) -> ClarifyState {
        if self.answer.is_some() {
            ClarifyState::Answered
        } else {
            ClarifyState::Asked
        }
    }
}

/// Request body to post a clarifying question.
#[derive(Deserialize)]
pub struct PostClarifyReq {
    pub question: String,
    pub addressee: String,
}

/// Request body to answer a clarification.
#[derive(Deserialize)]
pub struct AnswerReq {
    pub answer: String,
    pub answered_by: String,
}

/// In-memory store of clarifications, shared into the handlers.
#[derive(Clone, Default)]
pub struct ClarificationStore {
    items: Arc<Mutex<HashMap<String, Clarification>>>,
    counter: Arc<AtomicUsize>,
}

impl ClarificationStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// A store pre-seeded with a couple of open clarifications, so the cockpit's
    /// NEEDS YOU queue has real (not hardcoded) content on first load. These are
    /// genuine store entries, just seeded the way the spine is.
    pub fn seeded() -> Self {
        let store = Self::new();
        store.post(
            "CAM-1",
            "Should the CSV export include archived members, or only currently active ones?",
            "@maria-pm",
        );
        store.post(
            "CAM-2",
            "Should reminders use the org's timezone, or each member's own?",
            "@jdoe",
        );
        store
    }

    /// All OPEN (unanswered) clarifications across every story, oldest first. Drives
    /// the cockpit's NEEDS YOU queue.
    pub fn all_open(&self) -> Vec<Clarification> {
        let mut v: Vec<Clarification> = self
            .items
            .lock()
            .map(|g| g.values().filter(|c| c.is_open()).cloned().collect())
            .unwrap_or_default();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Post a new (open) clarification for a story; returns it.
    pub fn post(&self, story_id: &str, question: &str, addressee: &str) -> Clarification {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let c = Clarification {
            id: format!("clar-{n}"),
            story_id: story_id.to_string(),
            question: question.to_string(),
            addressee: addressee.to_string(),
            answer: None,
            answered_by: None,
            state: ClarifyState::Asked,
        };
        if let Ok(mut guard) = self.items.lock() {
            guard.insert(c.id.clone(), c.clone());
        }
        c
    }

    /// Record an answer on a clarification; returns the updated clarification, or
    /// `None` if the id is unknown.
    pub fn answer(&self, id: &str, answer: &str, answered_by: &str) -> Option<Clarification> {
        let mut guard = self.items.lock().ok()?;
        let c = guard.get_mut(id)?;
        c.answer = Some(answer.to_string());
        c.answered_by = Some(answered_by.to_string());
        // Persist the asked -> answered transition explicitly.
        c.state = ClarifyState::Answered;
        Some(c.clone())
    }

    /// All clarifications for a story, oldest first by id.
    pub fn for_story(&self, story_id: &str) -> Vec<Clarification> {
        let mut v: Vec<Clarification> = self
            .items
            .lock()
            .map(|g| {
                g.values()
                    .filter(|c| c.story_id == story_id)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_then_answer_round_trip() {
        let store = ClarificationStore::new();
        let c = store.post("CAM-1", "Currency for the export amounts?", "@maria-pm");
        assert!(c.is_open());
        assert_eq!(c.addressee, "@maria-pm");

        let open = store.for_story("CAM-1");
        assert_eq!(open.len(), 1);
        assert!(open[0].is_open());

        let answered = store
            .answer(&c.id, "USD, matching the billing currency.", "maria-pm")
            .expect("clarification exists");
        assert!(!answered.is_open());
        assert_eq!(answered.answered_by.as_deref(), Some("maria-pm"));

        // Answering an unknown id is a clean None, not a panic.
        assert!(store.answer("nope", "x", "y").is_none());
    }

    #[test]
    fn state_transitions_asked_to_answered() {
        let store = ClarificationStore::new();
        let c = store.post("CAM-1", "Currency?", "@maria-pm");
        // Freshly posted: explicit Asked state, consistent with the derived state.
        assert_eq!(c.state, ClarifyState::Asked);
        assert_eq!(c.state(), ClarifyState::Asked);
        assert!(c.is_open());

        let answered = store.answer(&c.id, "USD", "maria-pm").expect("exists");
        // Answering persists the transition on the stored field.
        assert_eq!(answered.state, ClarifyState::Answered);
        assert_eq!(answered.state(), ClarifyState::Answered);
        assert!(!answered.is_open());

        // And the transition is durable: re-reading from the store shows Answered.
        let reread = store
            .for_story("CAM-1")
            .into_iter()
            .find(|x| x.id == c.id)
            .expect("still present");
        assert_eq!(reread.state, ClarifyState::Answered);
    }

    #[test]
    fn state_field_serializes_as_snake_case() {
        let store = ClarificationStore::new();
        let c = store.post("CAM-1", "Currency?", "@maria-pm");
        let json = serde_json::to_string(&c).expect("serializes");
        assert!(json.contains("\"state\":\"asked\""), "got: {json}");

        let answered = store.answer(&c.id, "USD", "maria-pm").expect("exists");
        let json = serde_json::to_string(&answered).expect("serializes");
        assert!(json.contains("\"state\":\"answered\""), "got: {json}");
    }

    #[test]
    fn for_story_scopes_by_story() {
        let store = ClarificationStore::new();
        store.post("CAM-1", "q1", "@a");
        store.post("CAM-2", "q2", "@b");
        assert_eq!(store.for_story("CAM-1").len(), 1);
        assert_eq!(store.for_story("CAM-2").len(), 1);
        assert_eq!(store.for_story("CAM-9").len(), 0);
    }
}
