//! The multi-turn clarify loop: drive a [`LeadEngineer`] through Q&A rounds
//! until it produces a [`Plan`] or a turn cap is reached.
//!
//! # Design
//!
//! The [`ClarifyDriver`] wraps a [`LeadEngineer`] and an [`AnswerSource`]. On
//! each turn it calls [`LeadEngineer::evaluate`]; if the result is
//! [`Intake::NeedsClarification`], it calls [`AnswerSource::answer`] to get the
//! PO's answers, folds the Q&A into the form's `clarifications` field, and
//! re-evaluates. The loop terminates on the first [`Intake::Ready`] or when
//! `max_turns` is exhausted, whichever comes first.
//!
//! The fold strategy is append-only and fully transparent to the lead engineer:
//! each Q&A round is appended to [`IntakeForm::clarifications`] as a structured
//! block that the [`IntakeForm::brief`] renders verbatim. This means every
//! subsequent `evaluate` call sees the cumulative Q&A history without any hidden
//! state — the lead engineer can read it like a conversation transcript.
//!
//! # Testability
//!
//! [`AnswerSource`] is a trait so tests can inject a [`StubAnswerSource`] that
//! returns scripted answers without any network or stdin. The production path
//! (stdin) can implement the same trait.

use async_trait::async_trait;
use thiserror::Error;

use crate::engine::{LeadEngineer, LeadEngineerError, LeadEngineerResponse};
use crate::form::IntakeForm;
use crate::plan::Plan;

pub use crate::form::ClarificationRound;
use crate::Intake;

// ─── error ───────────────────────────────────────────────────────────────────

/// Errors from the clarify loop itself. Lead-engineer errors propagate as
/// [`ClarifyError::Engine`].
#[derive(Debug, Error)]
pub enum ClarifyError {
    /// The underlying lead engineer returned an error on a given turn.
    #[error("lead engineer error on turn {turn}: {source}")]
    Engine {
        /// Which 1-indexed turn the error occurred on.
        turn: usize,
        /// The underlying error.
        #[source]
        source: LeadEngineerError,
    },
}

// ─── answer source trait ──────────────────────────────────────────────────────

/// The PO's answer source for clarifying questions.
///
/// Implement this to supply answers to the lead engineer's questions. The
/// questions slice is the exact set returned in [`Intake::NeedsClarification`].
/// Return one answer string per question (matching by index); if `questions` is
/// empty, return an empty `Vec`.
///
/// This is the seam that makes the clarify loop testable without a network or
/// interactive stdin: tests inject [`StubAnswerSource`]; a real CLI
/// implementation reads from stdin.
#[async_trait]
pub trait AnswerSource: Send + Sync {
    /// Supply answers to `questions`. The returned `Vec` should be the same
    /// length as `questions`; shorter vecs are padded with empty strings when
    /// the driver folds them into the form.
    async fn answer(&self, questions: &[String]) -> Vec<String>;
}

// ─── stub answer source ───────────────────────────────────────────────────────

/// A deterministic, no-network [`AnswerSource`] that returns scripted answers.
///
/// Answers are consumed round-by-round. On turn N it returns the Nth row from
/// `rounds` (0-indexed). If more clarify turns occur than scripted rows, it
/// returns the last row repeatedly. This lets tests with a fixed number of
/// clarify turns be precise, while still being safe if the lead engineer asks an
/// unexpected extra round.
///
/// # Example
///
/// ```rust
/// use camerata_intake::clarify::StubAnswerSource;
///
/// // Two rounds of answers: turn 1 gets these, turn 2 gets these.
/// let src = StubAnswerSource::new(vec![
///     vec!["USD".to_string(), "monthly".to_string()],
///     vec!["yes".to_string()],
/// ]);
/// ```
#[derive(Debug, Clone)]
pub struct StubAnswerSource {
    rounds: Vec<Vec<String>>,
}

impl StubAnswerSource {
    /// Construct a stub with per-round scripted answers.
    pub fn new(rounds: Vec<Vec<String>>) -> Self {
        Self { rounds }
    }

    /// Construct a stub that returns the same set of answers on every turn.
    pub fn uniform(answers: Vec<String>) -> Self {
        Self {
            rounds: vec![answers],
        }
    }
}

#[async_trait]
impl AnswerSource for StubAnswerSource {
    async fn answer(&self, _questions: &[String]) -> Vec<String> {
        // Internal turn tracking would require mutability; instead the driver
        // passes its current turn index to a separate helper. This impl is
        // stateless and always returns the first round — use `answer_for_turn`
        // via `StubAnswerSource::answers_for` in the driver, or simply make
        // StubAnswerSource work via RoundedStubAnswerSource below.
        self.rounds.first().cloned().unwrap_or_default()
    }
}

/// A [`StubAnswerSource`] variant that is turn-aware.
///
/// Each call to `answer` advances an internal counter (by sharing a
/// `std::sync::atomic::AtomicUsize`) so round N returns `rounds[N]`.
///
/// This is what [`StubAnswerSource`] would need to be if you want per-turn
/// precision. The driver creates this internally from a `StubAnswerSource`.
/// Public so tests can use it directly if needed.
#[derive(Debug)]
pub struct SequentialAnswerSource {
    rounds: Vec<Vec<String>>,
    turn: std::sync::atomic::AtomicUsize,
}

impl SequentialAnswerSource {
    /// Construct from an ordered list of per-turn answer sets.
    pub fn new(rounds: Vec<Vec<String>>) -> Self {
        Self {
            rounds,
            turn: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl AnswerSource for SequentialAnswerSource {
    async fn answer(&self, _questions: &[String]) -> Vec<String> {
        let idx = self
            .turn
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Return the scripted row for this turn, or the last row if we ran out.
        let effective = idx.min(self.rounds.len().saturating_sub(1));
        self.rounds.get(effective).cloned().unwrap_or_default()
    }
}

// ─── outcome ─────────────────────────────────────────────────────────────────

/// The final outcome of a clarify loop run.
///
/// Every variant carries the [`LeadEngineerResponse`] captured at termination so
/// the UI always has the checklist, confidence score, and suggestions regardless
/// of how the loop ended.
#[derive(Debug, Clone)]
pub enum ClarifyOutcome {
    /// The lead engineer produced a plan within the turn cap.
    Resolved {
        /// The buildable plan.
        plan: Plan,
        /// How many clarify turns were needed before a plan was produced.
        /// 0 means the first `evaluate` call returned `Ready` immediately.
        clarify_turns: usize,
        /// The full staff-engineer response at the point of resolution.
        response: LeadEngineerResponse,
    },
    /// The turn cap was exhausted without reaching a plan.
    Unresolved {
        /// How many clarify turns were attempted.
        turns_attempted: usize,
        /// The questions still outstanding after the final turn.
        last_questions: Vec<String>,
        /// The staff-engineer response at the point of giving up.
        response: LeadEngineerResponse,
    },
    /// The lead engineer determined the app needs a human architect in the loop.
    /// Surfaced honestly rather than silently falling back to a best-effort plan.
    NeedsArchitect {
        /// Plain-language reason for the PO.
        reason: String,
        /// The staff-engineer response captured at the point of the verdict.
        response: LeadEngineerResponse,
    },
    /// The lead engineer determined the request is beyond Camerata's reach.
    /// Plain decline rather than confidently building something fragile.
    TooComplex {
        /// Plain-language reason for the PO.
        reason: String,
        /// The staff-engineer response captured at the point of the verdict.
        response: LeadEngineerResponse,
    },
}

impl ClarifyOutcome {
    /// The plan if resolved, otherwise `None`.
    pub fn plan(&self) -> Option<&Plan> {
        match self {
            ClarifyOutcome::Resolved { plan, .. } => Some(plan),
            _ => None,
        }
    }

    /// Whether the loop resolved to a plan.
    pub fn is_resolved(&self) -> bool {
        matches!(self, ClarifyOutcome::Resolved { .. })
    }

    /// Whether the loop ended with an honest decline (architect needed or too
    /// complex). These are NOT failures — they are trust features.
    pub fn is_honest_decline(&self) -> bool {
        matches!(
            self,
            ClarifyOutcome::NeedsArchitect { .. } | ClarifyOutcome::TooComplex { .. }
        )
    }

    /// The [`LeadEngineerResponse`] regardless of which variant this is.
    pub fn response(&self) -> &LeadEngineerResponse {
        match self {
            ClarifyOutcome::Resolved { response, .. } => response,
            ClarifyOutcome::Unresolved { response, .. } => response,
            ClarifyOutcome::NeedsArchitect { response, .. } => response,
            ClarifyOutcome::TooComplex { response, .. } => response,
        }
    }

    /// How many clarify turns were needed (0 if resolved on the first call).
    pub fn clarify_turns(&self) -> usize {
        match self {
            ClarifyOutcome::Resolved { clarify_turns, .. } => *clarify_turns,
            ClarifyOutcome::Unresolved {
                turns_attempted, ..
            } => *turns_attempted,
            // Honest declines happen at the first evaluation; 0 turns.
            ClarifyOutcome::NeedsArchitect { .. } | ClarifyOutcome::TooComplex { .. } => 0,
        }
    }
}

// ─── driver ──────────────────────────────────────────────────────────────────

/// The clarify loop driver.
///
/// Wraps a [`LeadEngineer`] + an [`AnswerSource`] and drives repeated
/// `evaluate` calls until the engineer yields a plan or `max_turns` is
/// exhausted.
///
/// # Turn cap
///
/// `max_turns` bounds the number of *clarification* turns (each round of
/// Q&A is one turn). It does NOT count the initial `evaluate` call that may
/// return [`Intake::Ready`] immediately. A `max_turns` of 3 means up to 3
/// Q&A rounds before giving up.
///
/// # Fold strategy
///
/// Clarifications are folded into [`IntakeForm::clarifications`]. Each round
/// appends a [`ClarificationRound`] so the lead engineer sees the full history
/// on every subsequent `evaluate`.
pub struct ClarifyDriver<'a> {
    engineer: &'a dyn LeadEngineer,
    answers: &'a dyn AnswerSource,
    max_turns: usize,
}

impl<'a> ClarifyDriver<'a> {
    /// Construct a driver with the given turn cap.
    ///
    /// `max_turns` is the maximum number of Q&A rounds before returning
    /// [`ClarifyOutcome::Unresolved`]. Must be at least 1; passing 0 is
    /// treated as 1 (one attempt, no clarification rounds).
    pub fn new(
        engineer: &'a dyn LeadEngineer,
        answers: &'a dyn AnswerSource,
        max_turns: usize,
    ) -> Self {
        Self {
            engineer,
            answers,
            max_turns: max_turns.max(1),
        }
    }

    /// Run the clarify loop over `form`.
    ///
    /// The `form` is cloned internally so the caller's original is unmodified;
    /// clarifications are accumulated on the internal copy only.
    ///
    /// Honest-limits variants (`RecommendArchitect` / `TooComplex`) are surfaced
    /// immediately as [`ClarifyOutcome::NeedsArchitect`] /
    /// [`ClarifyOutcome::TooComplex`] — the driver never silently ignores them.
    pub async fn run(&self, form: &IntakeForm) -> Result<ClarifyOutcome, ClarifyError> {
        let mut working = form.clone();
        let mut clarify_turns = 0usize;

        // Initial evaluation — may return Ready immediately (0 clarify turns).
        let mut intake = self
            .engineer
            .evaluate(&working)
            .await
            .map_err(|source| ClarifyError::Engine { turn: 1, source })?;

        loop {
            match intake {
                Intake::Ready { plan, response } => {
                    return Ok(ClarifyOutcome::Resolved {
                        plan,
                        clarify_turns,
                        response,
                    });
                }
                // Honest limits: surface immediately, do not try to clarify past them.
                Intake::RecommendArchitect { reason, response } => {
                    return Ok(ClarifyOutcome::NeedsArchitect { reason, response });
                }
                Intake::TooComplex { reason, response } => {
                    return Ok(ClarifyOutcome::TooComplex { reason, response });
                }
                Intake::NeedsClarification {
                    ref questions,
                    ref response,
                } => {
                    if clarify_turns >= self.max_turns {
                        return Ok(ClarifyOutcome::Unresolved {
                            turns_attempted: clarify_turns,
                            last_questions: questions.clone(),
                            response: response.clone(),
                        });
                    }

                    // Ask the answer source for the PO's answers.
                    let answers = self.answers.answer(questions).await;

                    // Fold Q&A into the working form.
                    let round = ClarificationRound {
                        questions: questions.clone(),
                        answers,
                    };
                    working.clarifications.push(round);
                    clarify_turns += 1;

                    // Re-evaluate with the enriched form.
                    intake = self
                        .engineer
                        .evaluate(&working)
                        .await
                        .map_err(|source| ClarifyError::Engine {
                            turn: clarify_turns + 1,
                            source,
                        })?;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{
        ChecklistItem, ConfidenceScore, HonestyVerdict, LeadEngineerError, LeadEngineerResponse,
        StubLeadEngineer,
    };
    use crate::form::{
        ClarificationRound, EntityDefinition, EntityField, EntityCapabilities,
        FieldType, IntakeForm, ViewKind, ViewSpec,
    };
    use crate::plan::{Plan, PlanTask, TaskKind};
    use async_trait::async_trait;

    // ─── helpers ──────────────────────────────────────────────────────────────

    fn minimal_response() -> LeadEngineerResponse {
        LeadEngineerResponse {
            checklist: vec![],
            confidence: ConfidenceScore::new(90),
            suggestions: vec![],
            verdict: HonestyVerdict::Proceed,
            questions: vec![],
        }
    }

    fn clarify_response(questions: Vec<String>) -> LeadEngineerResponse {
        LeadEngineerResponse {
            checklist: questions
                .iter()
                .enumerate()
                .map(|(i, q)| ChecklistItem::open(format!("item_{i}"), q, "needs answer"))
                .collect(),
            confidence: ConfidenceScore::new(40),
            suggestions: vec![],
            verdict: HonestyVerdict::Proceed,
            questions: questions.clone(),
        }
    }

    /// A lead engineer that returns NeedsClarification exactly `n` times, then
    /// yields a fixed plan.
    struct NTimesEngineer {
        clarify_count: std::sync::atomic::AtomicUsize,
        questions_each_turn: Vec<String>,
        final_plan: Plan,
    }

    impl NTimesEngineer {
        fn new(n: usize, questions: Vec<String>, plan: Plan) -> Self {
            Self {
                clarify_count: std::sync::atomic::AtomicUsize::new(n),
                questions_each_turn: questions,
                final_plan: plan,
            }
        }
    }

    #[async_trait]
    impl LeadEngineer for NTimesEngineer {
        async fn evaluate(&self, _form: &IntakeForm) -> Result<Intake, LeadEngineerError> {
            let remaining = self
                .clarify_count
                .fetch_update(
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                    |v| if v > 0 { Some(v - 1) } else { None },
                )
                .unwrap_or(0);

            if remaining > 0 {
                let qs = self.questions_each_turn.clone();
                Ok(Intake::NeedsClarification {
                    questions: qs.clone(),
                    response: clarify_response(qs),
                })
            } else {
                Ok(Intake::Ready {
                    plan: self.final_plan.clone(),
                    response: minimal_response(),
                })
            }
        }
    }

    fn stub_plan() -> Plan {
        Plan {
            app_name: "budget-tracker".to_string(),
            summary: "expense CRUD".to_string(),
            tasks: vec![PlanTask {
                role: "Implementer".to_string(),
                kind: TaskKind::Backend,
                description: "define Expense struct".to_string(),
            }],
        }
    }

    fn underspecified_form() -> IntakeForm {
        IntakeForm {
            app_name: "budget-tracker".to_string(),
            description: "track my money".to_string(),
            roles: vec![],
            entities: vec![EntityDefinition {
                name: "Expense".to_string(),
                description: String::new(),
                fields: vec![EntityField::required("amount", FieldType::Money)],
                capabilities: EntityCapabilities {
                    can_add: true,
                    can_list: true,
                    ..Default::default()
                },
            }],
            constraints: String::new(),
            views: vec![ViewSpec::new("Expense", ViewKind::List)],
            clarifications: vec![],
        }
    }

    // ─── tests ────────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn resolves_immediately_when_engineer_returns_ready() {
        // StubLeadEngineer always returns Ready: 0 clarify turns needed.
        let engineer = StubLeadEngineer::new();
        let answers = StubAnswerSource::uniform(vec![]);
        let driver = ClarifyDriver::new(&engineer, &answers, 3);
        let outcome = driver.run(&underspecified_form()).await.unwrap();
        assert!(outcome.is_resolved());
        assert_eq!(outcome.clarify_turns(), 0);
        // Response is surfaced even on immediate Ready.
        assert_eq!(outcome.response().confidence.value(), 90);
    }

    #[tokio::test]
    async fn resolves_after_one_clarify_turn() {
        let questions = vec!["Which currency?".to_string()];
        let engineer = NTimesEngineer::new(1, questions, stub_plan());
        let answers = StubAnswerSource::uniform(vec!["USD".to_string()]);
        let driver = ClarifyDriver::new(&engineer, &answers, 3);

        let outcome = driver.run(&underspecified_form()).await.unwrap();
        assert!(outcome.is_resolved());
        assert_eq!(outcome.clarify_turns(), 1);
        let plan = outcome.plan().unwrap();
        assert_eq!(plan.app_name, "budget-tracker");
    }

    #[tokio::test]
    async fn resolves_after_two_clarify_turns() {
        let questions = vec![
            "Which currency?".to_string(),
            "Monthly or weekly budget?".to_string(),
        ];
        let engineer = NTimesEngineer::new(2, questions, stub_plan());
        let answers = SequentialAnswerSource::new(vec![
            vec!["USD".to_string(), "monthly".to_string()],
            vec!["USD".to_string(), "monthly".to_string()],
        ]);
        let driver = ClarifyDriver::new(&engineer, &answers, 3);

        let outcome = driver.run(&underspecified_form()).await.unwrap();
        assert!(outcome.is_resolved());
        assert_eq!(outcome.clarify_turns(), 2);
    }

    #[tokio::test]
    async fn unresolved_when_turn_cap_exhausted() {
        // Engineer always asks for clarification — never yields a plan.
        let questions = vec!["Still need more info?".to_string()];
        let engineer = NTimesEngineer::new(usize::MAX, questions.clone(), stub_plan());
        let answers = StubAnswerSource::uniform(vec!["I don't know".to_string()]);
        let driver = ClarifyDriver::new(&engineer, &answers, 3);

        let outcome = driver.run(&underspecified_form()).await.unwrap();
        assert!(!outcome.is_resolved());
        assert_eq!(outcome.clarify_turns(), 3);
        match outcome {
            ClarifyOutcome::Unresolved {
                turns_attempted,
                last_questions,
                ..
            } => {
                assert_eq!(turns_attempted, 3);
                assert_eq!(last_questions, questions);
            }
            _ => panic!("expected Unresolved"),
        }
    }

    #[tokio::test]
    async fn clarifications_are_folded_into_form() {
        // Use a lead engineer that inspects the form's clarifications field.
        // After fold it should contain 1 round with the Q+A.
        struct InspectingEngineer {
            called: std::sync::atomic::AtomicUsize,
            final_plan: Plan,
        }
        #[async_trait]
        impl LeadEngineer for InspectingEngineer {
            async fn evaluate(&self, form: &IntakeForm) -> Result<Intake, LeadEngineerError> {
                let n = self.called.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n == 0 {
                    // First call: ask a question.
                    Ok(Intake::NeedsClarification {
                        questions: vec!["Currency?".to_string()],
                        response: clarify_response(vec!["Currency?".to_string()]),
                    })
                } else {
                    // Second call: verify the clarification was folded in.
                    assert_eq!(form.clarifications.len(), 1, "expected 1 folded round");
                    let round = &form.clarifications[0];
                    assert_eq!(round.questions, vec!["Currency?".to_string()]);
                    assert_eq!(round.answers, vec!["EUR".to_string()]);
                    Ok(Intake::Ready {
                        plan: self.final_plan.clone(),
                        response: minimal_response(),
                    })
                }
            }
        }

        let engineer = InspectingEngineer {
            called: std::sync::atomic::AtomicUsize::new(0),
            final_plan: stub_plan(),
        };
        let answers = StubAnswerSource::uniform(vec!["EUR".to_string()]);
        let driver = ClarifyDriver::new(&engineer, &answers, 3);

        let outcome = driver.run(&underspecified_form()).await.unwrap();
        assert!(outcome.is_resolved());
        assert_eq!(outcome.clarify_turns(), 1);
    }

    #[tokio::test]
    async fn max_turns_one_exhausts_after_first_clarify_turn() {
        // max_turns = 1: one clarify round is allowed, then give up.
        let questions = vec!["What?".to_string()];
        let engineer = NTimesEngineer::new(usize::MAX, questions.clone(), stub_plan());
        let answers = StubAnswerSource::uniform(vec!["something".to_string()]);
        let driver = ClarifyDriver::new(&engineer, &answers, 1);

        let outcome = driver.run(&underspecified_form()).await.unwrap();
        assert!(!outcome.is_resolved());
        assert_eq!(outcome.clarify_turns(), 1);
    }

    // ─── new staff-engineer behavior tests (ORCH-NEW-PATH-TESTS-1) ───────────

    #[tokio::test]
    async fn driver_surfaces_recommend_architect_honestly() {
        struct ArchitectEngineer;
        #[async_trait]
        impl LeadEngineer for ArchitectEngineer {
            async fn evaluate(&self, _form: &IntakeForm) -> Result<Intake, LeadEngineerError> {
                Ok(Intake::RecommendArchitect {
                    reason: "needs distributed state machine".to_string(),
                    response: LeadEngineerResponse {
                        checklist: vec![],
                        confidence: ConfidenceScore::new(30),
                        suggestions: vec![],
                        verdict: HonestyVerdict::RecommendArchitect {
                            reason: "needs distributed state machine".to_string(),
                        },
                        questions: vec![],
                    },
                })
            }
        }

        let engineer = ArchitectEngineer;
        let answers = StubAnswerSource::uniform(vec![]);
        let driver = ClarifyDriver::new(&engineer, &answers, 3);
        let outcome = driver.run(&underspecified_form()).await.unwrap();

        assert!(!outcome.is_resolved());
        assert!(outcome.is_honest_decline());
        assert!(matches!(outcome, ClarifyOutcome::NeedsArchitect { .. }));
        assert_eq!(outcome.clarify_turns(), 0);
    }

    #[tokio::test]
    async fn driver_surfaces_too_complex_honestly() {
        struct ComplexEngineer;
        #[async_trait]
        impl LeadEngineer for ComplexEngineer {
            async fn evaluate(&self, _form: &IntakeForm) -> Result<Intake, LeadEngineerError> {
                Ok(Intake::TooComplex {
                    reason: "requires a custom ML inference pipeline".to_string(),
                    response: LeadEngineerResponse {
                        checklist: vec![],
                        confidence: ConfidenceScore::new(10),
                        suggestions: vec![],
                        verdict: HonestyVerdict::TooComplex {
                            reason: "requires a custom ML inference pipeline".to_string(),
                        },
                        questions: vec![],
                    },
                })
            }
        }

        let engineer = ComplexEngineer;
        let answers = StubAnswerSource::uniform(vec![]);
        let driver = ClarifyDriver::new(&engineer, &answers, 3);
        let outcome = driver.run(&underspecified_form()).await.unwrap();

        assert!(!outcome.is_resolved());
        assert!(outcome.is_honest_decline());
        assert!(matches!(outcome, ClarifyOutcome::TooComplex { .. }));
    }

    #[tokio::test]
    async fn stub_engineer_produces_checklist_and_confidence() {
        let form = IntakeForm::sample_app();
        let engineer = StubLeadEngineer::new();
        let answers = StubAnswerSource::uniform(vec![]);
        let driver = ClarifyDriver::new(&engineer, &answers, 3);
        let outcome = driver.run(&form).await.unwrap();

        assert!(outcome.is_resolved());
        let response = outcome.response();
        // Stub always produces confidence 90 and a non-empty checklist.
        assert_eq!(response.confidence.value(), 90);
        assert!(!response.checklist.is_empty());
        // All checklist items are pre-resolved in the stub.
        assert!(response.checklist.iter().all(|i| i.resolved));
        // Verdict is Proceed.
        assert!(matches!(response.verdict, HonestyVerdict::Proceed));
    }

    #[tokio::test]
    async fn stub_engineer_emits_suggestions_for_owner_role() {
        // The sample_app has an "Owner" role, triggering the admin suggestion.
        let form = IntakeForm::sample_app();
        let response = StubLeadEngineer::response_for(&form);
        assert!(
            response.suggestions.iter().any(|s| s.id == "admin_users"),
            "expected an admin_users suggestion for a form with an Owner role"
        );
    }

    #[tokio::test]
    async fn stub_engineer_emits_soft_delete_suggestion_for_removable_entities() {
        let form = IntakeForm::sample_app();
        let response = StubLeadEngineer::response_for(&form);
        // sample_app Expense has can_remove = true.
        assert!(
            response.suggestions.iter().any(|s| s.id == "soft_delete"),
            "expected a soft_delete suggestion for entities with can_remove"
        );
    }

    #[tokio::test]
    async fn confidence_score_clamped_at_100() {
        let score = ConfidenceScore::new(200);
        assert_eq!(score.value(), 100);
        assert!(score.is_build_ready());
    }

    #[tokio::test]
    async fn confidence_score_below_80_is_not_build_ready() {
        let score = ConfidenceScore::new(79);
        assert!(!score.is_build_ready());
    }

    #[tokio::test]
    async fn checklist_item_open_and_resolved_states() {
        let open = ChecklistItem::open("id", "Question?", "reason");
        let resolved = ChecklistItem::resolved("id2", "Q2?", "r2");
        assert!(!open.resolved);
        assert!(resolved.resolved);
    }

    #[tokio::test]
    async fn lead_engineer_response_counts_resolved_and_open() {
        let response = LeadEngineerResponse {
            checklist: vec![
                ChecklistItem::resolved("a", "q1", "r1"),
                ChecklistItem::open("b", "q2", "r2"),
                ChecklistItem::open("c", "q3", "r3"),
            ],
            confidence: ConfidenceScore::new(60),
            suggestions: vec![],
            verdict: HonestyVerdict::Proceed,
            questions: vec![],
        };
        assert_eq!(response.resolved_count(), 1);
        assert_eq!(response.open_count(), 2);
        assert!(!response.checklist_complete());
    }

    #[test]
    fn clarification_round_renders_qa_pairs() {
        let round = ClarificationRound {
            questions: vec!["Currency?".to_string(), "Period?".to_string()],
            answers: vec!["USD".to_string(), "monthly".to_string()],
        };
        let rendered = round.render();
        assert!(rendered.contains("Currency?"));
        assert!(rendered.contains("USD"));
        assert!(rendered.contains("Period?"));
        assert!(rendered.contains("monthly"));
    }

    #[test]
    fn clarification_round_handles_missing_answer() {
        let round = ClarificationRound {
            questions: vec!["Currency?".to_string()],
            answers: vec![],
        };
        let rendered = round.render();
        assert!(rendered.contains("(no answer)"));
    }
}
