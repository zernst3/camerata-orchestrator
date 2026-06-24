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
use std::path::PathBuf;
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

/// One selectable option on a structured clarification: a label and a short
/// benefit/drawback description (the `AskUserQuestion` UX). Free-text-only
/// questions have an empty option list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClarifyOption {
    /// The short, selectable label (what the answer records).
    pub label: String,
    /// A one-line benefit/drawback description shown under the label so the user
    /// can weigh the choice.
    pub description: String,
}

/// A structured answer to a clarification: the selected option label(s) plus an
/// optional free-text ("Other") note. A pure free-text answer has an empty
/// `selected` and a `free_text`.
#[derive(Clone, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ClarifyAnswer {
    /// The option label(s) the user picked. One for single-select, possibly many
    /// for multi-select, empty for a free-text-only answer.
    #[serde(default)]
    pub selected: Vec<String>,
    /// The free-text ("Other") note, when the question allowed it (or for a pure
    /// free-text question).
    #[serde(default)]
    pub free_text: Option<String>,
}

impl ClarifyAnswer {
    /// Render a human-readable one-line summary (selected labels joined with `; `,
    /// then the free-text appended). This is what populates the back-compat
    /// `answer` string the queue + tracker write-back display.
    pub fn summary(&self) -> String {
        let mut parts: Vec<String> = self.selected.clone();
        if let Some(ft) = self.free_text.as_ref() {
            let ft = ft.trim();
            if !ft.is_empty() {
                parts.push(ft.to_string());
            }
        }
        parts.join("; ")
    }
}

/// One clarifying question on a story: who it is addressed to, the structured
/// options (if any), and the answer once it comes back.
#[derive(Clone, Serialize, Deserialize)]
pub struct Clarification {
    pub id: String,
    pub story_id: String,
    pub question: String,
    /// Who the question is addressed to (the per-question pick: a teammate, "you",
    /// or a free-typed handle). Not a standing "PO" role.
    pub addressee: String,
    /// The structured options to choose from (the `AskUserQuestion` UX). Empty for
    /// a pure free-text question. `#[serde(default)]` so older persisted JSON
    /// (which had no options) deserializes cleanly.
    #[serde(default)]
    pub options: Vec<ClarifyOption>,
    /// Whether more than one option may be selected (checkboxes vs. radio).
    #[serde(default)]
    pub multi_select: bool,
    /// Whether the "Other" free-text escape is offered. Defaults to `true` so a
    /// pure free-text question (empty options + free-text) is the natural fallback,
    /// and so older persisted JSON (no field) keeps the free-text leg.
    #[serde(default = "default_true")]
    pub allow_free_text: bool,
    /// The human-readable answer summary (selected labels + free-text), kept for
    /// back-compat, the NEEDS YOU queue display, and the tracker write-back.
    pub answer: Option<String>,
    /// The structured answer (selected labels + free-text). `None` while open.
    /// `#[serde(default)]` so older persisted JSON deserializes.
    #[serde(default)]
    pub answer_selection: Option<ClarifyAnswer>,
    pub answered_by: Option<String>,
    /// The explicit, persisted lifecycle state. Always consistent with `answer`:
    /// `Answered` iff `answer.is_some()`. Serialized so clients can branch on it
    /// directly rather than re-deriving from the nullable `answer`.
    pub state: ClarifyState,
}

/// Serde default for `allow_free_text` (true = the "Other" escape is on by default).
fn default_true() -> bool {
    true
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

/// Request body to post a clarifying question. The structured fields are optional:
/// when `options` is absent/empty the question is a pure free-text question (the
/// old behaviour), so existing callers keep working.
#[derive(Deserialize)]
pub struct PostClarifyReq {
    pub question: String,
    pub addressee: String,
    #[serde(default)]
    pub options: Vec<ClarifyOption>,
    #[serde(default)]
    pub multi_select: bool,
    /// Defaults to `true` (the "Other" escape on) when absent, so an old free-text
    /// post still records a free-text question.
    #[serde(default = "default_true")]
    pub allow_free_text: bool,
}

/// Request body to answer a clarification. `selected`/`free_text` are the structured
/// answer; `answer` is the legacy free-text field. When `selected` and `free_text`
/// are both empty/absent, the handler falls back to the legacy `answer` string, so
/// existing callers keep working.
#[derive(Deserialize)]
pub struct AnswerReq {
    #[serde(default)]
    pub answer: String,
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default)]
    pub free_text: Option<String>,
    pub answered_by: String,
}

/// Store of clarifications, shared into the handlers. In-memory by default
/// (`new`/`seeded`); when constructed via [`ClarificationStore::at`] it also
/// rehydrates from and flushes to a JSON file, so open questions + their answers
/// survive a restart (the resume guarantee).
#[derive(Clone, Default)]
pub struct ClarificationStore {
    items: Arc<Mutex<HashMap<String, Clarification>>>,
    counter: Arc<AtomicUsize>,
    /// The backing JSON file, when persistent. `None` = in-memory only.
    path: Option<Arc<PathBuf>>,
}

impl ClarificationStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Persist to (and rehydrate from) `path`. Open clarifications and their answers
    /// survive a restart, so the user can leave and resume at any open question.
    ///
    /// The `counter` is seeded past the highest existing `clar-N` id so a reopened
    /// store never re-issues an id that's already on disk.
    pub fn at(path: PathBuf) -> Self {
        let items: HashMap<String, Clarification> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        // Resume the id counter past anything already persisted.
        let max_n = items
            .keys()
            .filter_map(|k| k.strip_prefix("clar-"))
            .filter_map(|n| n.parse::<usize>().ok())
            .max()
            .unwrap_or(0);
        Self {
            items: Arc::new(Mutex::new(items)),
            counter: Arc::new(AtomicUsize::new(max_n)),
            path: Some(Arc::new(path)),
        }
    }

    /// Write the current map to the backing file, if persistent. Best-effort: a
    /// write failure is silent (the in-memory state stays authoritative). Called
    /// after every mutation. Must NOT be called while holding the `items` lock.
    fn flush(&self) {
        let Some(p) = &self.path else { return };
        let Ok(map) = self.items.lock() else { return };
        if let Ok(s) = serde_json::to_string(&*map) {
            let _ = std::fs::write(p.as_ref(), s);
        }
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

    /// Open clarifications whose `story_id` resolves to a repo in `repos` (the active
    /// project's repos). ISOLATION (A9): the global [`Self::all_open`] leaks every
    /// project's NEEDS-YOU queue. Clarifications whose story_id has no resolvable repo
    /// (drafts) are EXCLUDED.
    pub fn all_open_for_project(&self, repos: &[String]) -> Vec<Clarification> {
        let mut v: Vec<Clarification> = self
            .items
            .lock()
            .map(|g| {
                g.values()
                    .filter(|c| c.is_open())
                    .filter(|c| {
                        crate::repo_from_story_id(&c.story_id)
                            .is_some_and(|r| repos.iter().any(|p| p == &r))
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| a.id.cmp(&b.id));
        v
    }

    /// Post a new (open) free-text clarification for a story; returns it. This is the
    /// back-compat shim: it delegates to [`Self::post_structured`] with no options and
    /// the free-text escape on.
    pub fn post(&self, story_id: &str, question: &str, addressee: &str) -> Clarification {
        self.post_structured(story_id, question, addressee, Vec::new(), false, true)
    }

    /// Post a new (open) STRUCTURED clarification for a story; returns it. `options`
    /// empty + `allow_free_text` true is a pure free-text question (the back-compat
    /// case). Flushes to disk if persistent.
    pub fn post_structured(
        &self,
        story_id: &str,
        question: &str,
        addressee: &str,
        options: Vec<ClarifyOption>,
        multi_select: bool,
        allow_free_text: bool,
    ) -> Clarification {
        let n = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        let c = Clarification {
            id: format!("clar-{n}"),
            story_id: story_id.to_string(),
            question: question.to_string(),
            addressee: addressee.to_string(),
            options,
            multi_select,
            allow_free_text,
            answer: None,
            answer_selection: None,
            answered_by: None,
            state: ClarifyState::Asked,
        };
        if let Ok(mut guard) = self.items.lock() {
            guard.insert(c.id.clone(), c.clone());
        }
        self.flush();
        c
    }

    /// Record a free-text answer on a clarification; returns the updated clarification,
    /// or `None` if the id is unknown. Back-compat shim: stores the text as both the
    /// structured free-text and the summary.
    pub fn answer(&self, id: &str, answer: &str, answered_by: &str) -> Option<Clarification> {
        let sel = ClarifyAnswer {
            selected: Vec::new(),
            free_text: Some(answer.to_string()),
        };
        self.answer_structured(id, sel, answered_by)
    }

    /// Record a STRUCTURED answer on a clarification; returns the updated clarification,
    /// or `None` if the id is unknown. Sets both the structured `answer_selection` and
    /// the human-readable `answer` summary, then flushes if persistent.
    pub fn answer_structured(
        &self,
        id: &str,
        selection: ClarifyAnswer,
        answered_by: &str,
    ) -> Option<Clarification> {
        let updated = {
            let mut guard = self.items.lock().ok()?;
            let c = guard.get_mut(id)?;
            c.answer = Some(selection.summary());
            c.answer_selection = Some(selection);
            c.answered_by = Some(answered_by.to_string());
            // Persist the asked -> answered transition explicitly.
            c.state = ClarifyState::Answered;
            c.clone()
        };
        self.flush();
        Some(updated)
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
    fn structured_post_then_answer_round_trip() {
        let store = ClarificationStore::new();
        let c = store.post_structured(
            "CAM-1",
            "Which timezone for reminders?",
            "@maria-pm",
            vec![
                ClarifyOption {
                    label: "Org timezone".into(),
                    description: "One consistent send time; simpler to reason about.".into(),
                },
                ClarifyOption {
                    label: "Member timezone".into(),
                    description: "Reminders land at a sensible local hour per member.".into(),
                },
            ],
            false,
            true,
        );
        assert!(c.is_open());
        assert_eq!(c.options.len(), 2);
        assert!(!c.multi_select);
        assert!(c.allow_free_text);

        let answered = store
            .answer_structured(
                &c.id,
                ClarifyAnswer {
                    selected: vec!["Member timezone".into()],
                    free_text: Some("but default to org tz if unknown".into()),
                },
                "maria-pm",
            )
            .expect("clarification exists");
        assert!(!answered.is_open());
        assert_eq!(answered.state, ClarifyState::Answered);
        let sel = answered.answer_selection.as_ref().expect("structured answer");
        assert_eq!(sel.selected, vec!["Member timezone".to_string()]);
        assert_eq!(sel.free_text.as_deref(), Some("but default to org tz if unknown"));
        // The summary reflects selected labels + free-text.
        assert_eq!(
            answered.answer.as_deref(),
            Some("Member timezone; but default to org tz if unknown")
        );
    }

    #[test]
    fn structured_multi_select() {
        let store = ClarificationStore::new();
        let c = store.post_structured(
            "CAM-1",
            "Which columns to include?",
            "you",
            vec![
                ClarifyOption { label: "Name".into(), description: "d".into() },
                ClarifyOption { label: "Email".into(), description: "d".into() },
                ClarifyOption { label: "Phone".into(), description: "d".into() },
            ],
            true,
            false,
        );
        assert!(c.multi_select);
        assert!(!c.allow_free_text);

        let answered = store
            .answer_structured(
                &c.id,
                ClarifyAnswer {
                    selected: vec!["Name".into(), "Email".into()],
                    free_text: None,
                },
                "zach",
            )
            .expect("exists");
        let sel = answered.answer_selection.as_ref().unwrap();
        assert_eq!(sel.selected.len(), 2);
        assert_eq!(answered.answer.as_deref(), Some("Name; Email"));
    }

    #[test]
    fn free_text_only_back_compat() {
        // The old post/answer shims still work and produce a free-text question/answer.
        let store = ClarificationStore::new();
        let c = store.post("CAM-1", "Anything else?", "you");
        assert!(c.options.is_empty());
        assert!(c.allow_free_text);
        assert!(!c.multi_select);

        let answered = store
            .answer(&c.id, "Yes, also export PDFs.", "zach")
            .expect("exists");
        assert_eq!(answered.answer.as_deref(), Some("Yes, also export PDFs."));
        let sel = answered.answer_selection.as_ref().unwrap();
        assert!(sel.selected.is_empty());
        assert_eq!(sel.free_text.as_deref(), Some("Yes, also export PDFs."));
    }

    #[test]
    fn persistence_survives_reopen_resume_guarantee() {
        let dir = std::env::temp_dir().join(format!(
            "cam-clarify-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clarifications.json");

        let posted_id;
        {
            let store = ClarificationStore::at(path.clone());
            let c = store.post_structured(
                "CAM-7",
                "Ship behind a flag?",
                "@maria-pm",
                vec![
                    ClarifyOption { label: "Flag".into(), description: "safer rollout".into() },
                    ClarifyOption { label: "Direct".into(), description: "simpler".into() },
                ],
                false,
                true,
            );
            posted_id = c.id.clone();
            store
                .answer_structured(
                    &c.id,
                    ClarifyAnswer {
                        selected: vec!["Flag".into()],
                        free_text: None,
                    },
                    "maria-pm",
                )
                .unwrap();
            // store dropped here
        }

        // Reopen at the same path: the question + its answer survived the restart.
        let reopened = ClarificationStore::at(path.clone());
        let restored = reopened
            .for_story("CAM-7")
            .into_iter()
            .find(|x| x.id == posted_id)
            .expect("clarification survived restart");
        assert_eq!(restored.question, "Ship behind a flag?");
        assert_eq!(restored.options.len(), 2);
        assert_eq!(restored.state, ClarifyState::Answered);
        assert_eq!(restored.answer.as_deref(), Some("Flag"));
        assert_eq!(
            restored.answer_selection.as_ref().unwrap().selected,
            vec!["Flag".to_string()]
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn serde_default_loads_legacy_json_without_options() {
        // A clarification persisted before the structured fields existed: no `options`,
        // `multi_select`, `allow_free_text`, or `answer_selection`. It must still load.
        let legacy = r#"{
            "clar-1": {
                "id": "clar-1",
                "story_id": "CAM-1",
                "question": "Currency?",
                "addressee": "@maria-pm",
                "answer": "USD",
                "answered_by": "maria-pm",
                "state": "answered"
            }
        }"#;
        let map: HashMap<String, Clarification> =
            serde_json::from_str(legacy).expect("legacy JSON deserializes");
        let c = map.get("clar-1").expect("present");
        assert!(c.options.is_empty());
        assert!(!c.multi_select);
        // allow_free_text defaults to true for legacy records.
        assert!(c.allow_free_text);
        assert!(c.answer_selection.is_none());
        assert_eq!(c.answer.as_deref(), Some("USD"));
    }

    #[test]
    fn summary_reflects_selected_and_free_text() {
        let a = ClarifyAnswer {
            selected: vec!["A".into(), "B".into()],
            free_text: Some("note".into()),
        };
        assert_eq!(a.summary(), "A; B; note");

        let only_sel = ClarifyAnswer { selected: vec!["A".into()], free_text: None };
        assert_eq!(only_sel.summary(), "A");

        let only_ft = ClarifyAnswer {
            selected: vec![],
            free_text: Some("free".into()),
        };
        assert_eq!(only_ft.summary(), "free");

        // Whitespace-only free-text is dropped.
        let blank_ft = ClarifyAnswer {
            selected: vec!["A".into()],
            free_text: Some("   ".into()),
        };
        assert_eq!(blank_ft.summary(), "A");
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
