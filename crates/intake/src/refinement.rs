//! The refinement session: the single back-and-forth primitive, reused in every
//! lifecycle moment.
//!
//! Camerata's whole consumer flow is ONE primitive alternating with execution.
//! That primitive is the [`RefinementSession`]: the AI reviews the current
//! artifacts (user stories, clarifications), edits stories, raises product
//! suggestions, and asks clarifying questions; the user edits, answers, adds, and
//! deletes; the [`ConfidenceScore`] updates; and this repeats until the USER is
//! happy with the confidence (or bypasses it). See `docs/CONSUMER_UX.md`.
//!
//! The same session runs in three [`RefinementContext`]s, which is why it is a
//! first-class type rather than a pre-build-only loop:
//!
//! - [`RefinementContext::PreBuild`] — the onboarding document has been turned
//!   into user stories and the PO refines them into a ready-to-build spec.
//! - [`RefinementContext::MidBuild`] — a builder agent hit a real question that
//!   changes the outcome and escalated; execution pauses on this session until
//!   it resolves, then resumes.
//! - [`RefinementContext::PostBuild`] — the PO QA-tested the draft and filed
//!   structured bug reports; those become "bug stories" that this session folds
//!   in before re-execution.
//!
//! The session owns the artifacts under refinement and exposes PURE state
//! transitions (RUST-PURE-STATE-TRANSITIONS-1): each user or AI action returns a
//! mutated session (or mutates in place) and records a [`RefinementTurn`], so the
//! whole back-and-forth is a replayable, persistable log. The aggregate that ties
//! a project's frozen onboarding document to its living stories and its session
//! history is [`crate::project::Project`].

use serde::{Deserialize, Serialize};

use crate::engine::{ConfidenceScore, HonestyVerdict, ProductSuggestion};
use crate::form::ClarificationRound;
use crate::story::{StoryId, UserStory};

// ─── actor ───────────────────────────────────────────────────────────────────

/// Who performed a refinement turn. The audit trail distinguishes AI edits from
/// user edits, exactly as the persistence layer's `EditActor` does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    /// The lead engineer (AI) reviewed and proposed changes.
    Ai,
    /// The human Product Owner edited, answered, added, or deleted.
    User,
}

// ─── escalation + bug report (the non-pre-build seeds) ───────────────────────

/// A question a builder agent raises mid-execution that changes the outcome and
/// cannot be guessed. It pauses execution and seeds a
/// [`RefinementContext::MidBuild`] session scoped to exactly this question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Escalation {
    /// The plan task (by description or id) that raised the question.
    pub from_task: String,
    /// The plain-language question for the PO. Example: "For the export, did you
    /// want a spreadsheet or a PDF?"
    pub question: String,
}

impl Escalation {
    /// Construct an escalation from a builder task.
    pub fn new(from_task: impl Into<String>, question: impl Into<String>) -> Self {
        Self {
            from_task: from_task.into(),
            question: question.into(),
        }
    }
}

/// A structured, post-build bug report. The bug form FORCES this shape so the
/// report is something the agents can act on, not a vague "it's broken." Each
/// report becomes a "bug story" ([`crate::story::StoryOrigin::BugReport`]) in a
/// [`RefinementContext::PostBuild`] session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BugReport {
    /// Which screen or feature the problem is on.
    pub location: String,
    /// What the user did (the steps).
    pub did: String,
    /// What the user expected to happen.
    pub expected: String,
    /// What actually happened instead.
    pub happened: String,
}

impl BugReport {
    /// Construct a structured bug report (all four fields required by the form).
    pub fn new(
        location: impl Into<String>,
        did: impl Into<String>,
        expected: impl Into<String>,
        happened: impl Into<String>,
    ) -> Self {
        Self {
            location: location.into(),
            did: did.into(),
            expected: expected.into(),
            happened: happened.into(),
        }
    }

    /// Render the report as a plain-language block the agents read verbatim.
    pub fn render(&self) -> String {
        format!(
            "On «{}»:\n  I did: {}\n  I expected: {}\n  Instead: {}\n",
            self.location, self.did, self.expected, self.happened
        )
    }
}

// ─── context ─────────────────────────────────────────────────────────────────

/// Which lifecycle moment a [`RefinementSession`] is running in. The session
/// transitions are identical across all three; only what SEEDS the session
/// differs. This enum is what makes the refinement session a single primitive
/// reused everywhere rather than three bespoke flows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "context", rename_all = "snake_case")]
pub enum RefinementContext {
    /// Before the first build: onboarding document to ready-to-build spec.
    PreBuild,
    /// During execution: a builder escalation paused the build on this session.
    MidBuild {
        /// The escalation that opened the session.
        escalation: Escalation,
    },
    /// After a build: QA + structured bug reports drive the session.
    PostBuild {
        /// The bug reports that seeded this session.
        bugs: Vec<BugReport>,
    },
}

impl RefinementContext {
    /// A short, stable label for the context (used in UI + persistence).
    pub fn label(&self) -> &'static str {
        match self {
            RefinementContext::PreBuild => "pre_build",
            RefinementContext::MidBuild { .. } => "mid_build",
            RefinementContext::PostBuild { .. } => "post_build",
        }
    }

    /// Whether this context pauses an in-flight execution (only `MidBuild` does).
    pub fn pauses_execution(&self) -> bool {
        matches!(self, RefinementContext::MidBuild { .. })
    }
}

// ─── turn ────────────────────────────────────────────────────────────────────

/// One contribution to the back-and-forth. Each turn records who acted, a
/// plain-language note of what happened, and the confidence AFTER the turn, so
/// the session's history is a replayable transcript with a confidence curve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefinementTurn {
    /// Who acted this turn.
    pub actor: Actor,
    /// Plain-language summary of what changed this turn.
    pub note: String,
    /// The confidence score after this turn was applied.
    pub confidence_after: ConfidenceScore,
}

// ─── session state ───────────────────────────────────────────────────────────

/// The state of a refinement session. Only [`SessionState::Converged`] permits
/// the lifecycle to advance to execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// The AI reviewed and is waiting on the user (open questions / suggestions).
    AwaitingUser,
    /// The user edited / answered and is waiting on the AI to review again.
    AwaitingReview,
    /// The user is happy with the confidence and closed the session. The
    /// lifecycle may now advance (to execution, or to publish).
    Converged,
}

// ─── the AI's review output ──────────────────────────────────────────────────

/// What the lead engineer produces when it reviews a session: edited or new
/// stories, new clarifying questions, new product suggestions, an updated
/// confidence, an honesty verdict, and a plain-language note. Folding a review
/// into a session ([`RefinementSession::apply_review`]) records one AI turn.
///
/// This is the AI side of a refinement turn. The user side is the
/// add/edit/delete/answer methods on [`RefinementSession`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RefinementReview {
    /// Stories the AI proposes to add or replace (matched by [`StoryId`]). An
    /// existing story with the same id is replaced; a new id is appended.
    #[serde(default)]
    pub upserted_stories: Vec<UserStory>,
    /// Story ids the AI proposes to remove.
    #[serde(default)]
    pub removed_story_ids: Vec<StoryId>,
    /// New clarifying questions for the user this turn.
    #[serde(default)]
    pub questions: Vec<String>,
    /// New product-level suggestions raised this turn.
    #[serde(default)]
    pub suggestions: Vec<ProductSuggestion>,
    /// The AI's updated confidence after this review.
    #[serde(default)]
    pub confidence: ConfidenceScore,
    /// The honesty verdict at this point (may flip to architect/too-complex).
    #[serde(default)]
    pub verdict: HonestyVerdict,
    /// A plain-language note recorded as the AI turn's summary.
    #[serde(default)]
    pub note: String,
}

// ─── the session ─────────────────────────────────────────────────────────────

/// One refinement session: the unit of back-and-forth, reused in all three
/// [`RefinementContext`]s.
///
/// The session owns the artifacts under refinement (stories, clarifications,
/// suggestions) and the running confidence. Every method is a pure-ish state
/// transition that also records a [`RefinementTurn`] so the session is a
/// replayable, persistable log of the conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefinementSession {
    /// Stable session id, unique within a project.
    pub id: String,
    /// Which lifecycle moment this session is running in.
    pub context: RefinementContext,
    /// The living spec under refinement: the user stories. After the onboarding
    /// document is frozen these (plus bug stories) are the source of truth.
    pub stories: Vec<UserStory>,
    /// The Q&A record: every clarification round, append-only.
    pub clarifications: Vec<ClarificationRound>,
    /// Product-level suggestions raised so far, each optionally referencing a
    /// story.
    pub suggestions: Vec<ProductSuggestion>,
    /// The running confidence, the signal the loop converges toward.
    pub confidence: ConfidenceScore,
    /// The honesty verdict at the latest review.
    pub verdict: HonestyVerdict,
    /// The back-and-forth log (one entry per AI or user turn).
    pub turns: Vec<RefinementTurn>,
    /// The session's state.
    pub state: SessionState,
}

impl RefinementSession {
    /// Open a session in the given context, seeded with `stories`. Starts in
    /// [`SessionState::AwaitingReview`] (the AI reviews first) at confidence 0.
    ///
    /// For a [`RefinementContext::PostBuild`] context, each [`BugReport`] is
    /// converted into a "bug story" appended to `stories`, so every entry point
    /// (direct `open`, [`Self::post_build`], or [`crate::project::Project`]) gets
    /// the same first-class bug stories.
    pub fn open(
        id: impl Into<String>,
        context: RefinementContext,
        mut stories: Vec<UserStory>,
    ) -> Self {
        if let RefinementContext::PostBuild { bugs } = &context {
            for (i, bug) in bugs.iter().enumerate() {
                stories.push(UserStory::from_bug(
                    format!("bug_{i}"),
                    format!("Fix: {}", bug.location),
                    "Anyone using the app",
                    vec![format!(
                        "When I {}, I should get «{}» instead of «{}»",
                        bug.did, bug.expected, bug.happened
                    )],
                ));
            }
        }
        Self {
            id: id.into(),
            context,
            stories,
            clarifications: Vec::new(),
            suggestions: Vec::new(),
            confidence: ConfidenceScore::new(0),
            verdict: HonestyVerdict::Proceed,
            turns: Vec::new(),
            state: SessionState::AwaitingReview,
        }
    }

    /// Open a pre-build session (the common case): onboarding to ready spec.
    pub fn pre_build(id: impl Into<String>, stories: Vec<UserStory>) -> Self {
        Self::open(id, RefinementContext::PreBuild, stories)
    }

    /// Open a mid-build session scoped to a builder escalation.
    pub fn mid_build(id: impl Into<String>, escalation: Escalation, stories: Vec<UserStory>) -> Self {
        Self::open(id, RefinementContext::MidBuild { escalation }, stories)
    }

    /// Open a post-build session seeded with QA bug reports. Each bug becomes a
    /// "bug story" (the conversion happens in [`Self::open`]).
    pub fn post_build(id: impl Into<String>, bugs: Vec<BugReport>, stories: Vec<UserStory>) -> Self {
        Self::open(id, RefinementContext::PostBuild { bugs }, stories)
    }

    // ── user-side transitions ────────────────────────────────────────────────

    /// USER adds a story. Records a user turn and moves to `AwaitingReview`.
    pub fn add_story(&mut self, story: UserStory) {
        self.stories.push(story);
        self.user_turn("Added a story");
    }

    /// USER removes a story by id. Returns whether a story was removed. Records a
    /// user turn when something changed.
    pub fn remove_story(&mut self, id: &StoryId) -> bool {
        let before = self.stories.len();
        self.stories.retain(|s| &s.id != id);
        let removed = self.stories.len() != before;
        if removed {
            self.user_turn("Removed a story");
        }
        removed
    }

    /// USER adds or replaces a story (by id). Records a user turn.
    pub fn upsert_story(&mut self, story: UserStory) {
        upsert(&mut self.stories, story);
        self.user_turn("Edited a story");
    }

    /// USER answers the latest open questions. If the last clarification round is
    /// open (the AI posed questions but they are unanswered), this FILLS that
    /// round's answers in place; otherwise it appends a new round. Either way it
    /// records a user turn and moves to `AwaitingReview`. Filling-in-place keeps
    /// the transcript one-round-per-exchange (the AI asks, the user answers the
    /// same round) rather than doubling it.
    pub fn answer(&mut self, round: ClarificationRound) {
        match self.clarifications.last_mut() {
            Some(last) if last.answers.is_empty() => {
                last.questions = round.questions;
                last.answers = round.answers;
            }
            _ => self.clarifications.push(round),
        }
        self.user_turn("Answered clarifications");
    }

    /// USER declares they are happy with the current confidence and closes the
    /// session. Moves to [`SessionState::Converged`]. This is the bypass too: the
    /// PO can converge at ANY confidence; the score only makes the trade-off
    /// legible.
    pub fn converge(&mut self) {
        self.state = SessionState::Converged;
        self.turns.push(RefinementTurn {
            actor: Actor::User,
            note: "Marked the session ready".to_string(),
            confidence_after: self.confidence,
        });
    }

    // ── AI-side transition ───────────────────────────────────────────────────

    /// Fold an AI [`RefinementReview`] into the session: apply story upserts and
    /// removals, append new questions as suggestions/clarification prompts, add
    /// new suggestions, update confidence + verdict, record one AI turn, and move
    /// to [`SessionState::AwaitingUser`] when there is anything for the user to do
    /// (open questions or suggestions), else leave it for the user to converge.
    pub fn apply_review(&mut self, review: RefinementReview) {
        for story in review.upserted_stories {
            upsert(&mut self.stories, story);
        }
        for id in &review.removed_story_ids {
            self.stories.retain(|s| &s.id != id);
        }
        self.suggestions.extend(review.suggestions);
        self.confidence = review.confidence;
        self.verdict = review.verdict;

        let has_open_work = !review.questions.is_empty();
        self.turns.push(RefinementTurn {
            actor: Actor::Ai,
            note: if review.note.is_empty() {
                "Reviewed the spec".to_string()
            } else {
                review.note
            },
            confidence_after: self.confidence,
        });

        // The questions become the open items the user is asked to answer next.
        // We keep them on the session via a clarification round with no answers
        // yet, so the UI transcript and the persisted state both carry them.
        if has_open_work {
            self.clarifications.push(ClarificationRound {
                questions: review.questions,
                answers: vec![],
            });
            self.state = SessionState::AwaitingUser;
        } else {
            // Nothing outstanding: the user may converge.
            self.state = SessionState::AwaitingUser;
        }
    }

    // ── queries ──────────────────────────────────────────────────────────────

    /// Whether the session has converged (the user closed it).
    pub fn is_converged(&self) -> bool {
        matches!(self.state, SessionState::Converged)
    }

    /// Whether the honesty verdict still permits a build.
    pub fn can_build(&self) -> bool {
        self.verdict.can_build()
    }

    /// The number of stories currently under refinement.
    pub fn story_count(&self) -> usize {
        self.stories.len()
    }

    /// The latest open questions (the unanswered clarification round), if any.
    pub fn open_questions(&self) -> &[String] {
        match self.clarifications.last() {
            Some(round) if round.answers.is_empty() => &round.questions,
            _ => &[],
        }
    }

    // ── internals ────────────────────────────────────────────────────────────

    /// Record a user turn at the current confidence and set `AwaitingReview`.
    fn user_turn(&mut self, note: &str) {
        self.turns.push(RefinementTurn {
            actor: Actor::User,
            note: note.to_string(),
            confidence_after: self.confidence,
        });
        self.state = SessionState::AwaitingReview;
    }
}

/// Insert `story` into `stories`, replacing any existing story with the same id.
fn upsert(stories: &mut Vec<UserStory>, story: UserStory) {
    if let Some(slot) = stories.iter_mut().find(|s| s.id == story.id) {
        *slot = story;
    } else {
        stories.push(story);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ConfidenceScore;

    fn story(id: &str) -> UserStory {
        UserStory::from_investigation(id, "T", "W", vec!["I can do a thing".to_string()])
    }

    fn review(confidence: u8, questions: Vec<&str>) -> RefinementReview {
        RefinementReview {
            confidence: ConfidenceScore::new(confidence),
            questions: questions.into_iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn opens_pre_build_awaiting_review_at_zero_confidence() {
        let s = RefinementSession::pre_build("s1", vec![story("a")]);
        assert_eq!(s.context.label(), "pre_build");
        assert_eq!(s.state, SessionState::AwaitingReview);
        assert_eq!(s.confidence.value(), 0);
        assert_eq!(s.story_count(), 1);
        assert!(!s.context.pauses_execution());
    }

    #[test]
    fn mid_build_context_pauses_execution_and_carries_escalation() {
        let esc = Escalation::new("export task", "Spreadsheet or PDF?");
        let s = RefinementSession::mid_build("s2", esc.clone(), vec![]);
        assert_eq!(s.context.label(), "mid_build");
        assert!(s.context.pauses_execution());
        match &s.context {
            RefinementContext::MidBuild { escalation } => assert_eq!(escalation, &esc),
            _ => panic!("expected MidBuild"),
        }
    }

    #[test]
    fn post_build_turns_each_bug_into_a_bug_story() {
        let bugs = vec![
            BugReport::new("List screen", "clicked add", "a new row", "nothing"),
            BugReport::new("Edit screen", "saved", "it saved", "an error"),
        ];
        let s = RefinementSession::post_build("s3", bugs, vec![story("existing")]);
        assert_eq!(s.context.label(), "post_build");
        // 1 existing + 2 bug stories.
        assert_eq!(s.story_count(), 3);
        let bug_stories: Vec<_> = s
            .stories
            .iter()
            .filter(|st| st.origin == crate::story::StoryOrigin::BugReport)
            .collect();
        assert_eq!(bug_stories.len(), 2);
    }

    #[test]
    fn apply_review_upserts_stories_and_updates_confidence() {
        let mut s = RefinementSession::pre_build("s", vec![story("a")]);
        let mut r = review(70, vec!["Which currency?"]);
        r.upserted_stories = vec![
            UserStory::from_investigation("a", "Edited title", "W", vec!["new bullet".to_string()]),
            UserStory::from_investigation("b", "New story", "W", vec![]),
        ];
        s.apply_review(r);

        assert_eq!(s.confidence.value(), 70);
        // a was replaced (title changed), b appended.
        assert_eq!(s.story_count(), 2);
        assert_eq!(s.stories[0].title, "Edited title");
        // The AI turn was recorded and an open clarification round added.
        assert_eq!(s.turns.len(), 1);
        assert_eq!(s.turns[0].actor, Actor::Ai);
        assert_eq!(s.open_questions(), &["Which currency?".to_string()]);
        assert_eq!(s.state, SessionState::AwaitingUser);
    }

    #[test]
    fn apply_review_removes_stories_by_id() {
        let mut s = RefinementSession::pre_build("s", vec![story("a"), story("b")]);
        let mut r = review(50, vec![]);
        r.removed_story_ids = vec![StoryId::new("a")];
        s.apply_review(r);
        assert_eq!(s.story_count(), 1);
        assert_eq!(s.stories[0].id.as_str(), "b");
    }

    #[test]
    fn user_add_remove_upsert_record_turns_and_await_review() {
        let mut s = RefinementSession::pre_build("s", vec![]);
        s.add_story(story("a"));
        assert_eq!(s.state, SessionState::AwaitingReview);
        assert_eq!(s.turns.last().unwrap().actor, Actor::User);

        // upsert replaces, does not grow.
        s.upsert_story(UserStory::user_added("a", "Renamed", "W", vec![]));
        assert_eq!(s.story_count(), 1);
        assert_eq!(s.stories[0].title, "Renamed");

        assert!(s.remove_story(&StoryId::new("a")));
        assert_eq!(s.story_count(), 0);
        // removing a missing story returns false and records no extra turn.
        let turns_before = s.turns.len();
        assert!(!s.remove_story(&StoryId::new("nope")));
        assert_eq!(s.turns.len(), turns_before);
    }

    #[test]
    fn answer_folds_a_clarification_round() {
        let mut s = RefinementSession::pre_build("s", vec![]);
        s.apply_review(review(40, vec!["Which currency?"]));
        assert_eq!(s.open_questions().len(), 1);
        s.answer(ClarificationRound {
            questions: vec!["Which currency?".to_string()],
            answers: vec!["USD".to_string()],
        });
        // After answering, the last round has answers, so no open questions.
        assert!(s.open_questions().is_empty());
        assert_eq!(s.state, SessionState::AwaitingReview);
    }

    #[test]
    fn converge_is_the_only_path_to_ready_and_works_at_any_confidence() {
        let mut s = RefinementSession::pre_build("s", vec![story("a")]);
        // Bypass: converge even at low confidence.
        s.apply_review(review(35, vec![]));
        assert!(!s.is_converged());
        s.converge();
        assert!(s.is_converged());
        assert_eq!(s.state, SessionState::Converged);
        assert_eq!(s.turns.last().unwrap().actor, Actor::User);
        // The trade-off is legible: confidence stayed at 35.
        assert_eq!(s.confidence.value(), 35);
    }

    #[test]
    fn verdict_flip_blocks_build() {
        let mut s = RefinementSession::pre_build("s", vec![]);
        let mut r = review(20, vec![]);
        r.verdict = HonestyVerdict::TooComplex {
            reason: "needs an ML pipeline".to_string(),
        };
        s.apply_review(r);
        assert!(!s.can_build());
    }

    #[test]
    fn the_same_transitions_work_in_all_three_contexts() {
        // Proves the session is one primitive: identical transition sequence,
        // three different seeding contexts.
        let esc = Escalation::new("t", "q?");
        let bug = BugReport::new("l", "d", "e", "h");
        let sessions = vec![
            RefinementSession::pre_build("p", vec![story("a")]),
            RefinementSession::mid_build("m", esc, vec![story("a")]),
            RefinementSession::post_build("b", vec![bug], vec![story("a")]),
        ];
        for mut s in sessions {
            s.apply_review(review(80, vec![]));
            assert_eq!(s.confidence.value(), 80);
            s.converge();
            assert!(s.is_converged());
            assert!(s.can_build());
        }
    }

    #[test]
    fn session_round_trips_json() {
        let mut s = RefinementSession::pre_build("s", vec![story("a")]);
        s.apply_review(review(60, vec!["Q?"]));
        let json = serde_json::to_string(&s).unwrap();
        let back: RefinementSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn bug_report_renders_all_four_fields() {
        let bug = BugReport::new("List", "clicked add", "a row", "nothing");
        let r = bug.render();
        assert!(r.contains("List"));
        assert!(r.contains("clicked add"));
        assert!(r.contains("a row"));
        assert!(r.contains("nothing"));
    }
}
