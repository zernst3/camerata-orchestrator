//! The refinement reviewer: the AI side of a [`RefinementSession`], generalized
//! across all three contexts (pre-build, mid-build escalation, post-build bugs).
//!
//! Where [`crate::engine::LeadEngineer`] evaluates a one-shot [`IntakeForm`], a
//! [`RefinementReviewer`] reviews a LIVE session (its stories, its clarification
//! history, its context) and returns a [`RefinementReview`] the session folds in:
//! edited or new stories, fresh clarifying questions, product suggestions, an
//! updated confidence, and an honesty verdict. That makes the refinement loop the
//! single intelligent primitive the consumer flow is built on.
//!
//! Two implementations ship, mirroring the lead-engineer seam:
//! - [`StubRefinementReviewer`] — deterministic, no network. Confidence climbs as
//!   clarification rounds are answered; it raises one product suggestion and asks
//!   from a small derived question pool until it is confident. Used in tests and
//!   as the offline fallback so the loop always converges.
//! - [`ClaudeRefinementReviewer`] — the live `claude -p` review. Read-only,
//!   JSON-only, ungoverned (a planning call, not a worktree write).
//!
//! [`RefinementDriver`] runs the loop: review -> apply -> (answer) -> review ...
//! until the reviewer stops asking questions or a turn cap is hit. It reuses the
//! [`crate::clarify::AnswerSource`] seam so it is testable without a network.

use async_trait::async_trait;
use thiserror::Error;

use crate::clarify::AnswerSource;
use crate::engine::{ConfidenceScore, HonestyVerdict, ProductSuggestion};
use crate::form::{ClarificationRound, IntakeForm};
use crate::refinement::{RefinementReview, RefinementSession};
use crate::story::StoryId;

// ─── errors ──────────────────────────────────────────────────────────────────

/// Errors from running a refinement review (RUST-DOMAIN-6).
#[derive(Debug, Error)]
pub enum ReviewError {
    /// The `claude` process could not be spawned.
    #[error("failed to spawn `claude`: {0}")]
    Spawn(#[source] std::io::Error),
    /// `claude -p` exited non-zero.
    #[error("`claude -p` exited with status {status}: {stderr}")]
    NonZeroExit {
        /// The exit status.
        status: String,
        /// Captured stderr.
        stderr: String,
    },
    /// The CLI's outer JSON envelope did not parse.
    #[error("could not parse `claude -p` JSON envelope: {0}")]
    ParseEnvelope(#[source] serde_json::Error),
    /// The model's inner `result` text was not the review JSON we asked for.
    #[error("reviewer did not return a parseable review: {0}")]
    ParseReview(String),
}

// ─── the seam ────────────────────────────────────────────────────────────────

/// REVIEWER SEAM — review a live [`RefinementSession`] and produce a
/// [`RefinementReview`] to fold in. Provider / model / tier live behind this seam.
#[async_trait]
pub trait RefinementReviewer: Send + Sync {
    /// Review the session (in the light of the original form) and return the
    /// changes to apply this turn.
    async fn review(
        &self,
        session: &RefinementSession,
        form: &IntakeForm,
    ) -> Result<RefinementReview, ReviewError>;
}

// ─── deterministic stub ──────────────────────────────────────────────────────

/// The confidence the stub starts at, the moment it has read the brief but pinned
/// nothing down. Honest: a complete-but-vague spec is workable, not certain.
pub const STUB_CONFIDENCE_START: u8 = 55;
/// How much confidence each answered clarification round adds.
pub const STUB_CONFIDENCE_PER_ROUND: u8 = 12;
/// At or above this confidence the stub stops asking questions.
pub const STUB_CONFIDENCE_READY: u8 = 80;

/// A deterministic, no-network reviewer. Confidence climbs with each answered
/// clarification round; it raises a single product suggestion on the first turn
/// (an admin area, when the form implies login) and asks one derived question per
/// turn until it reaches [`STUB_CONFIDENCE_READY`]. This guarantees the loop
/// converges, which is what makes it a safe offline fallback.
#[derive(Debug, Default, Clone)]
pub struct StubRefinementReviewer;

impl StubRefinementReviewer {
    /// Construct the stub reviewer.
    pub fn new() -> Self {
        Self
    }

    /// Smart, FORM-DERIVED clarifying questions, in plain language, referencing the
    /// user's actual entity names. A real staff engineer reads the shape of the app
    /// and asks about the gaps it implies (money needs a currency, emails imply a
    /// privacy line, removable things imply soft-delete, links imply referential
    /// behavior, dates imply history). Capped at 4 so the loop stays tight. This is
    /// what makes the hero screen feel like a real engineer, deterministically.
    fn derived_questions(form: &IntakeForm) -> Vec<String> {
        use crate::form::FieldType;
        let mut qs: Vec<String> = Vec::new();

        // No roles declared at all: pin down who uses it first.
        if form.roles.is_empty() {
            qs.push(
                "Who are the kinds of people who use this, and what should each be able to do?"
                    .to_string(),
            );
        }

        // A money field needs a currency.
        if let Some(e) = form.entities.iter().find(|e| {
            e.fields
                .iter()
                .any(|f| matches!(f.field_type, FieldType::Money | FieldType::Decimal))
        }) {
            qs.push(format!(
                "What currency should the money amounts on {} be in?",
                e.name
            ));
        }

        // An email (or any contact detail) implies a privacy line.
        if let Some(e) = form.entities.iter().find(|e| {
            e.fields
                .iter()
                .any(|f| matches!(f.field_type, FieldType::Email))
        }) {
            qs.push(format!(
                "Who should be able to see the email on a {}: just you, or everyone?",
                e.name
            ));
        }

        // A removable thing implies a soft-delete decision.
        if let Some(e) = form.entities.iter().find(|e| e.capabilities.can_remove) {
            qs.push(format!(
                "When a {} is removed, should it be gone for good, or hidden but recoverable?",
                e.name
            ));
        }

        // A link between entities implies referential behavior.
        if let Some((e, target)) = form.entities.iter().find_map(|e| {
            e.fields.iter().find_map(|f| match &f.field_type {
                FieldType::LinkTo(t) => Some((e, t.clone())),
                _ => None,
            })
        }) {
            let target = if target.trim().is_empty() {
                "linked item".to_string()
            } else {
                target
            };
            qs.push(format!(
                "When a {target} is removed, what should happen to the {} records that point to it?",
                e.name
            ));
        }

        // A date implies a history-vs-drop-off decision.
        if qs.len() < 4 {
            if let Some(e) = form.entities.iter().find(|e| {
                e.fields
                    .iter()
                    .any(|f| matches!(f.field_type, FieldType::Date | FieldType::DateTime))
            }) {
                qs.push(format!(
                    "Should past {} records drop off the list on their own, or stay visible as history?",
                    e.name
                ));
            }
        }

        // Always have at least one question so the conversation has substance.
        if qs.is_empty() {
            qs.push(
                "Is there anything specific about how this should behave that I should pin down before building?"
                    .to_string(),
            );
        }

        qs.truncate(4);
        qs
    }
}

#[async_trait]
impl RefinementReviewer for StubRefinementReviewer {
    async fn review(
        &self,
        session: &RefinementSession,
        form: &IntakeForm,
    ) -> Result<RefinementReview, ReviewError> {
        // Confidence + readiness are driven by the form-derived checklist: ask one
        // smart question per turn until every derived question is answered.
        let answered = session
            .clarifications
            .iter()
            .filter(|r| !r.answers.is_empty())
            .count();
        let pool = Self::derived_questions(form);

        let questions = if answered < pool.len() {
            vec![pool[answered].clone()]
        } else {
            vec![]
        };

        // Confidence climbs as questions are answered; once the checklist is clear
        // it settles at a confident value (a real engineer is sure once the gaps
        // are closed).
        let base = (STUB_CONFIDENCE_START as usize + STUB_CONFIDENCE_PER_ROUND as usize * answered)
            .min(96);
        let confidence = if questions.is_empty() {
            ConfidenceScore::new(base.max(92) as u8)
        } else {
            ConfidenceScore::new(base as u8)
        };

        // On the very first turn, raise an admin suggestion when the form implies
        // login, pointing at the first role story.
        let mut suggestions = Vec::new();
        if answered == 0 {
            let implies_login = form.roles.iter().any(|r| {
                let n = r.name.to_lowercase();
                n.contains("admin") || n.contains("owner") || n.contains("member")
            });
            if implies_login {
                let story_ref = session
                    .stories
                    .iter()
                    .find(|s| s.id.as_str().starts_with("role_"))
                    .map(|s| s.id.clone())
                    .unwrap_or_else(|| StoryId::new("role_0"));
                suggestions.push(ProductSuggestion::for_story(
                    "admin_users",
                    story_ref,
                    "You have people who log in. Apps like this usually also need a private place \
                     to manage who has access and what they are allowed to do. Want me to include \
                     a simple users-and-permissions area?",
                    "Without it, adding or removing people means touching the database by hand.",
                ));
            }
        }

        let ready = questions.is_empty();
        Ok(RefinementReview {
            upserted_stories: vec![],
            removed_story_ids: vec![],
            questions,
            suggestions,
            confidence,
            verdict: HonestyVerdict::Proceed,
            note: if ready {
                "I have what I need. I'm confident I can build this well.".to_string()
            } else {
                "Reviewed your stories. A couple of things to pin down.".to_string()
            },
        })
    }
}

// ─── live Claude reviewer ────────────────────────────────────────────────────

/// The default model id the live review call uses.
pub const DEFAULT_REVIEWER_MODEL: &str = "claude-sonnet-4-6";

/// The REAL refinement reviewer: a headless `claude -p` call that reviews the
/// session and returns the review JSON. Read-only and ungoverned (it plans, it
/// does not build).
#[derive(Debug, Clone)]
pub struct ClaudeRefinementReviewer {
    model: String,
}

impl Default for ClaudeRefinementReviewer {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeRefinementReviewer {
    /// Construct with the default model.
    pub fn new() -> Self {
        Self {
            model: DEFAULT_REVIEWER_MODEL.to_string(),
        }
    }

    /// Construct with an explicit model id.
    pub fn with_model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }

    /// Render the session's stories + clarification history as a plain-language
    /// brief the model reviews. Pure + public for testing.
    pub fn render_session(session: &RefinementSession) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "CONTEXT: {}\n\nSTORIES:\n",
            session.context.label()
        ));
        for story in &session.stories {
            out.push_str(&story.render());
        }
        if !session.clarifications.is_empty() {
            out.push_str("\nCLARIFICATIONS SO FAR:\n");
            for round in &session.clarifications {
                out.push_str(&round.render());
            }
        }
        out
    }

    /// Build the review prompt. Pure + public so it is unit-testable without a
    /// process.
    pub fn build_prompt(session: &RefinementSession, form: &IntakeForm) -> String {
        format!(
            "You are a STAFF LEAD ENGINEER refining a small bespoke app WITH a \
             non-technical Product Owner. You are mid-conversation: review the \
             current user stories and the clarifications so far, and return the \
             changes to make this turn.\n\n\
             === ORIGINAL BRIEF ===\n{brief}\n=== END BRIEF ===\n\n\
             === CURRENT SESSION ===\n{session}\n=== END SESSION ===\n\n\
             === YOUR JOB ===\n\
             1. Propose any story edits or new stories (consumer-abstracted: who \
                it is for and a plain bulleted list of what they want to SEE and \
                DO, never technical contracts).\n\
             2. Ask only the clarifying questions you still genuinely need, in \
                plain language.\n\
             3. Raise product-level SUGGESTIONS the PO would not think of, each \
                referencing the story id it concerns.\n\
             4. Score your CONFIDENCE 0-100 (how ready you are to build well).\n\
             5. Give an honesty VERDICT: proceed / recommend_architect / \
                too_complex.\n\n\
             === OUTPUT FORMAT ===\n\
             Output ONLY one JSON object (no prose, no fences):\n\
             {{\n\
             \x20 \"upserted_stories\": [ {{ \"id\": string, \"title\": string, \
                  \"for_whom\": string, \"wants\": [string], \"so_that\": string|null, \
                  \"origin\": \"investigation\"|\"user_added\"|\"bug_report\" }} ],\n\
             \x20 \"removed_story_ids\": [string],\n\
             \x20 \"questions\": [string],\n\
             \x20 \"suggestions\": [ {{ \"id\": string, \"suggestion\": string, \
                  \"rationale\": string, \"story_id\": string|null }} ],\n\
             \x20 \"confidence\": number,\n\
             \x20 \"verdict\": {{ \"verdict\": \"proceed\" }} | \
                  {{ \"verdict\": \"recommend_architect\", \"reason\": string }} | \
                  {{ \"verdict\": \"too_complex\", \"reason\": string }},\n\
             \x20 \"note\": string\n\
             }}\n\
             Output the JSON object and nothing else.",
            brief = form.brief(),
            session = Self::render_session(session),
        )
    }

    /// Parse the model's inner `result` text into a [`RefinementReview`],
    /// tolerating surrounding prose / a fence. Pure + public for direct testing.
    pub fn parse_review(result_text: &str) -> Result<RefinementReview, ReviewError> {
        let json = extract_json_object(result_text).ok_or_else(|| {
            ReviewError::ParseReview(format!(
                "no JSON object found: {}",
                truncate(result_text, 200)
            ))
        })?;
        let mut review: RefinementReview = serde_json::from_str(json)
            .map_err(|e| ReviewError::ParseReview(format!("{e}; raw: {}", truncate(json, 200))))?;
        // Clamp confidence defensively (direct deserialize bypasses ::new).
        review.confidence = ConfidenceScore::new(review.confidence.value());
        Ok(review)
    }
}

#[async_trait]
impl RefinementReviewer for ClaudeRefinementReviewer {
    async fn review(
        &self,
        session: &RefinementSession,
        form: &IntakeForm,
    ) -> Result<RefinementReview, ReviewError> {
        let prompt = Self::build_prompt(session, form);
        let out = tokio::process::Command::new("claude")
            .arg("-p")
            .arg(&prompt)
            .arg("--model")
            .arg(&self.model)
            .arg("--allowedTools")
            .arg("")
            .arg("--dangerously-skip-permissions")
            .arg("--output-format")
            .arg("json")
            .output()
            .await
            .map_err(ReviewError::Spawn)?;
        if !out.status.success() {
            return Err(ReviewError::NonZeroExit {
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        let envelope: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(ReviewError::ParseEnvelope)?;
        let result_text = envelope["result"].as_str().unwrap_or_default();
        Self::parse_review(result_text)
    }
}

// ─── the driver ──────────────────────────────────────────────────────────────

/// The outcome of running the refinement loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefineOutcome {
    /// The reviewer stopped asking questions: the session is ready for the user to
    /// converge. Carries how many review turns ran.
    Ready {
        /// Number of review turns taken.
        turns: usize,
    },
    /// The turn cap was hit while the reviewer still had questions.
    CappedOut {
        /// Number of review turns taken.
        turns: usize,
    },
    /// The reviewer declined honestly (architect needed or too complex).
    Declined {
        /// Plain-language reason.
        reason: String,
    },
}

/// Drives the refinement loop over a session: review, apply, answer, repeat. Reuses
/// the [`AnswerSource`] seam so it is testable without a network or stdin.
pub struct RefinementDriver<'a> {
    reviewer: &'a dyn RefinementReviewer,
    answers: &'a dyn AnswerSource,
    max_turns: usize,
}

impl<'a> RefinementDriver<'a> {
    /// Construct a driver with a turn cap (>= 1).
    pub fn new(
        reviewer: &'a dyn RefinementReviewer,
        answers: &'a dyn AnswerSource,
        max_turns: usize,
    ) -> Self {
        Self {
            reviewer,
            answers,
            max_turns: max_turns.max(1),
        }
    }

    /// Run the loop, mutating `session` in place. Returns when the reviewer stops
    /// asking, the cap is hit, or the reviewer declines honestly.
    pub async fn run(
        &self,
        session: &mut RefinementSession,
        form: &IntakeForm,
    ) -> Result<RefineOutcome, ReviewError> {
        let mut turns = 0usize;
        loop {
            let review = self.reviewer.review(session, form).await?;
            // Capture a possible honest decline before moving `review`.
            let decline = match &review.verdict {
                HonestyVerdict::RecommendArchitect { reason }
                | HonestyVerdict::TooComplex { reason } => Some(reason.clone()),
                HonestyVerdict::Proceed => None,
            };
            session.apply_review(review);
            turns += 1;

            if let Some(reason) = decline {
                return Ok(RefineOutcome::Declined { reason });
            }

            let open = session.open_questions().to_vec();
            if open.is_empty() {
                return Ok(RefineOutcome::Ready { turns });
            }
            if turns >= self.max_turns {
                return Ok(RefineOutcome::CappedOut { turns });
            }

            // Answer the open questions and fold them in, then review again.
            let answers = self.answers.answer(&open).await;
            session.answer(ClarificationRound {
                questions: open,
                answers,
            });
        }
    }
}

// ─── helpers (shared shape with engine.rs) ───────────────────────────────────

/// Extract the first balanced top-level `{...}` JSON object span from `s`.
fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in s[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Truncate `s` to at most `n` chars for bounded error messages.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let t: String = s.chars().take(n).collect();
        format!("{t}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clarify::{SequentialAnswerSource, StubAnswerSource};
    use crate::refinement::{BugReport, Escalation, RefinementContext, RefinementSession};
    use crate::story::UserStory;

    fn form_with_owner() -> IntakeForm {
        IntakeForm::sample_app()
    }

    fn session_with_role_story(ctx: RefinementContext) -> RefinementSession {
        let stories = vec![UserStory::from_investigation(
            "role_0",
            "As the owner",
            "Owner",
            vec!["I can manage things".into()],
        )];
        RefinementSession::open("s", ctx, stories)
    }

    #[tokio::test]
    async fn stub_confidence_climbs_and_loop_converges() {
        let reviewer = StubRefinementReviewer::new();
        let answers =
            SequentialAnswerSource::new(vec![vec!["a".into()], vec!["b".into()], vec!["c".into()]]);
        let mut session = session_with_role_story(RefinementContext::PreBuild);
        let driver = RefinementDriver::new(&reviewer, &answers, 6);
        let outcome = driver.run(&mut session, &form_with_owner()).await.unwrap();

        // 55 -> 67 -> 79 -> 91: ready after enough answered rounds.
        match outcome {
            RefineOutcome::Ready { turns } => assert!(turns >= 3, "got {turns} turns"),
            other => panic!("expected Ready, got {other:?}"),
        }
        assert!(session.confidence.value() >= STUB_CONFIDENCE_READY);
        // The first turn raised the admin suggestion, referencing the role story.
        let admin = session
            .suggestions
            .iter()
            .find(|s| s.id == "admin_users")
            .unwrap();
        assert_eq!(admin.story_id.as_ref().unwrap().as_str(), "role_0");
    }

    #[tokio::test]
    async fn stub_review_is_pure_for_a_given_answered_count() {
        let reviewer = StubRefinementReviewer::new();
        let session = session_with_role_story(RefinementContext::PreBuild);
        let r1 = reviewer.review(&session, &form_with_owner()).await.unwrap();
        let r2 = reviewer.review(&session, &form_with_owner()).await.unwrap();
        assert_eq!(r1.confidence, r2.confidence);
        assert_eq!(r1.questions, r2.questions);
    }

    #[tokio::test]
    async fn stub_asks_form_specific_questions_naming_the_entity() {
        // sample_app has an Expense entity with a Money field and can_remove, so the
        // first derived question is about currency and names the real entity.
        let reviewer = StubRefinementReviewer::new();
        let session = RefinementSession::open("s", RefinementContext::PreBuild, vec![]);
        let review = reviewer.review(&session, &form_with_owner()).await.unwrap();
        let q = &review.questions[0];
        assert!(
            q.to_lowercase().contains("currency") && q.contains("Expense"),
            "expected a form-specific currency question naming Expense, got: {q}"
        );
    }

    #[tokio::test]
    async fn driver_caps_out_when_answers_never_satisfy() {
        // A reviewer that always asks and never gains confidence.
        struct StuckReviewer;
        #[async_trait]
        impl RefinementReviewer for StuckReviewer {
            async fn review(
                &self,
                _s: &RefinementSession,
                _f: &IntakeForm,
            ) -> Result<RefinementReview, ReviewError> {
                Ok(RefinementReview {
                    questions: vec!["still unsure?".into()],
                    confidence: ConfidenceScore::new(40),
                    ..Default::default()
                })
            }
        }
        let answers = StubAnswerSource::uniform(vec!["dunno".into()]);
        let mut session = session_with_role_story(RefinementContext::PreBuild);
        let driver = RefinementDriver::new(&StuckReviewer, &answers, 3);
        let outcome = driver.run(&mut session, &form_with_owner()).await.unwrap();
        assert_eq!(outcome, RefineOutcome::CappedOut { turns: 3 });
    }

    #[tokio::test]
    async fn driver_surfaces_honest_decline() {
        struct DecliningReviewer;
        #[async_trait]
        impl RefinementReviewer for DecliningReviewer {
            async fn review(
                &self,
                _s: &RefinementSession,
                _f: &IntakeForm,
            ) -> Result<RefinementReview, ReviewError> {
                Ok(RefinementReview {
                    verdict: HonestyVerdict::TooComplex {
                        reason: "needs an ML pipeline".into(),
                    },
                    ..Default::default()
                })
            }
        }
        let answers = StubAnswerSource::uniform(vec![]);
        let mut session = session_with_role_story(RefinementContext::PreBuild);
        let driver = RefinementDriver::new(&DecliningReviewer, &answers, 3);
        let outcome = driver.run(&mut session, &form_with_owner()).await.unwrap();
        assert!(matches!(outcome, RefineOutcome::Declined { .. }));
        assert!(!session.can_build());
    }

    #[tokio::test]
    async fn the_loop_runs_in_all_three_contexts() {
        let reviewer = StubRefinementReviewer::new();
        let answers =
            SequentialAnswerSource::new(vec![vec!["x".into()], vec!["y".into()], vec!["z".into()]]);
        let contexts = vec![
            RefinementContext::PreBuild,
            RefinementContext::MidBuild {
                escalation: Escalation::new("t", "q?"),
            },
            RefinementContext::PostBuild {
                bugs: vec![BugReport::new("l", "d", "e", "h")],
            },
        ];
        for ctx in contexts {
            let mut session = session_with_role_story(ctx);
            let driver = RefinementDriver::new(&reviewer, &answers, 6);
            let outcome = driver.run(&mut session, &form_with_owner()).await.unwrap();
            assert!(matches!(outcome, RefineOutcome::Ready { .. }));
        }
    }

    #[test]
    fn prompt_includes_brief_session_and_json_shape() {
        let session = session_with_role_story(RefinementContext::PreBuild);
        let prompt = ClaudeRefinementReviewer::build_prompt(&session, &form_with_owner());
        assert!(prompt.contains("STAFF LEAD ENGINEER"));
        assert!(prompt.contains("As the owner")); // story rendered into the session block
        assert!(prompt.contains("\"upserted_stories\""));
        assert!(prompt.contains("\"confidence\""));
        assert!(prompt.contains("\"verdict\""));
    }

    #[test]
    fn parse_review_reads_the_rich_json() {
        let raw = r#"{
            "upserted_stories": [
                {"id": "s1", "title": "See things", "for_whom": "Me",
                 "wants": ["I can see"], "so_that": null, "origin": "investigation"}
            ],
            "removed_story_ids": ["old"],
            "questions": ["which currency?"],
            "suggestions": [
                {"id": "admin", "suggestion": "add admin", "rationale": "needed", "story_id": "s1"}
            ],
            "confidence": 72,
            "verdict": {"verdict": "proceed"},
            "note": "looking good"
        }"#;
        let review = ClaudeRefinementReviewer::parse_review(raw).unwrap();
        assert_eq!(review.confidence.value(), 72);
        assert_eq!(review.upserted_stories.len(), 1);
        assert_eq!(review.removed_story_ids, vec![StoryId::new("old")]);
        assert_eq!(
            review.suggestions[0].story_id.as_ref().unwrap().as_str(),
            "s1"
        );
        assert!(matches!(review.verdict, HonestyVerdict::Proceed));
    }

    #[test]
    fn parse_review_clamps_out_of_range_confidence() {
        let raw = r#"{"confidence": 250, "verdict": {"verdict":"proceed"}}"#;
        let review = ClaudeRefinementReviewer::parse_review(raw).unwrap();
        assert_eq!(review.confidence.value(), 100);
    }

    #[test]
    fn parse_review_rejects_non_json() {
        let err = ClaudeRefinementReviewer::parse_review("nope").unwrap_err();
        assert!(matches!(err, ReviewError::ParseReview(_)));
    }
}
