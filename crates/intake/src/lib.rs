//! camerata-intake: the SECOND abstraction level (Product-Owner mode).
//!
//! Where the architect cockpit (the rest of the orchestrator) puts a principal
//! architect in front of the governed engine, this crate puts a *Product Owner*
//! in front of it. The human fills out a structured [`form::IntakeForm`] instead
//! of steering an investigation; an AI [`engine::LeadEngineer`] evaluates that
//! form as the lead engineer and emits a [`plan::Plan`] (the same shape the
//! architect would approve); and the governed fleet (in `camerata-core`) then
//! builds the plan's tasks under the gate. See `docs/PO_MODE.md` and VISION
//! sections 5 (P5), the "two abstraction levels" subsection, and 20.
//!
//! This crate makes NO orchestration decisions and runs NO checks. It owns four
//! bounded contexts (RUST-DOMAIN-1), one module each:
//!
//! - [`form`] — the PO intake schema (the form a Product Owner fills out).
//! - [`plan`] — the lead engineer's structured output (a buildable plan).
//! - [`engine`] — the [`engine::LeadEngineer`] seam + a real ([`engine::ClaudeLeadEngineer`])
//!   and a deterministic-fallback ([`engine::StubLeadEngineer`]) implementation.
//! - [`clarify`] — the multi-turn clarify loop ([`clarify::ClarifyDriver`]) that
//!   drives a [`engine::LeadEngineer`] through Q&A rounds until it yields a plan
//!   or a turn cap is hit. The [`clarify::AnswerSource`] trait makes the loop
//!   testable without network or stdin; [`clarify::StubAnswerSource`] and
//!   [`clarify::SequentialAnswerSource`] are the deterministic test doubles.
//!
//! The [`Intake`] enum models the clarify-loop state: a freshly-evaluated form is
//! either [`Intake::Ready`] (the lead engineer produced a plan) or
//! [`Intake::NeedsClarification`] (it has questions for the PO first). The
//! multi-turn clarify loop in [`clarify::ClarifyDriver`] consumes
//! `NeedsClarification` rounds, folding each Q&A into the form's
//! `clarifications` field and re-evaluating, until the engineer is satisfied.

pub mod clarify;
pub mod engine;
pub mod form;
pub mod plan;
pub mod project;
pub mod refinement;
pub mod review;
pub mod story;

pub use clarify::{
    AnswerSource, ClarifyDriver, ClarifyError, ClarifyOutcome, SequentialAnswerSource,
    StubAnswerSource,
};
pub use engine::{
    ChecklistItem, ClaudeLeadEngineer, ConfidenceScore, HonestyVerdict, LeadEngineer,
    LeadEngineerError, LeadEngineerResponse, ProductSuggestion, StubLeadEngineer,
};
pub use form::{
    ClarificationRound,
    // new open-ended types
    EntityDefinition, EntityCapabilities, EntityField, FieldType, UserRole,
    // legacy backward-compat types (kept for CLI + existing code)
    Entity, Field, FieldKind, ViewKind, ViewSpec,
    // the main form
    IntakeForm,
};
pub use plan::{Plan, PlanTask, TaskKind};
pub use project::{LifecycleError, Phase, Project};
pub use refinement::{
    Actor, BugReport, Escalation, RefinementContext, RefinementReview, RefinementSession,
    RefinementTurn, SessionState,
};
pub use review::{
    ClaudeRefinementReviewer, RefineOutcome, RefinementDriver, RefinementReviewer, ReviewError,
    StubRefinementReviewer,
};
pub use story::{StoryId, StoryOrigin, UserStory};

use serde::{Deserialize, Serialize};

/// The result of running a [`LeadEngineer`] over an [`IntakeForm`].
///
/// This is the clarify-loop state machine in one type. Every variant carries a
/// [`LeadEngineerResponse`] so the UI always has the checklist, confidence
/// score, suggestions, and honesty verdict regardless of which branch is taken.
///
/// Variants:
///
/// - [`Intake::Ready`] — the lead engineer produced a buildable plan. The
///   checklist is complete (or the PO bypassed it) and confidence is high.
/// - [`Intake::NeedsClarification`] — the lead engineer has open checklist
///   items and wants answers before committing to a plan.
/// - [`Intake::RecommendArchitect`] — the app has real architectural complexity
///   that warrants a human architect in the loop. Camerata surfaces this
///   honestly rather than guessing.
/// - [`Intake::TooComplex`] — the request is beyond what Camerata can build
///   well on its own. Plain decline rather than building something fragile.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Intake {
    /// The lead engineer understood the form well enough to produce a plan.
    Ready {
        /// The buildable plan.
        plan: Plan,
        /// The full staff-engineer response (checklist, confidence, suggestions).
        response: LeadEngineerResponse,
    },
    /// The lead engineer needs answers before it will commit to a plan.
    NeedsClarification {
        /// The questions for the Product Owner on this turn.
        questions: Vec<String>,
        /// The full staff-engineer response (checklist so far, confidence so
        /// far, suggestions already raised).
        response: LeadEngineerResponse,
    },
    /// The app has architectural complexity that warrants a human in the loop.
    /// The lead engineer surfaces this honestly rather than guessing.
    RecommendArchitect {
        /// Plain-language explanation for the PO.
        reason: String,
        /// The staff-engineer response captured at the point of the assessment.
        response: LeadEngineerResponse,
    },
    /// The request is beyond what Camerata can build well on its own. Plain
    /// decline rather than confidently building something fragile.
    TooComplex {
        /// Plain-language explanation for the PO.
        reason: String,
        /// The staff-engineer response captured at the point of the assessment.
        response: LeadEngineerResponse,
    },
}

impl Intake {
    /// The plan, if this intake is [`Intake::Ready`]; otherwise `None`.
    pub fn plan(&self) -> Option<&Plan> {
        match self {
            Intake::Ready { plan, .. } => Some(plan),
            _ => None,
        }
    }

    /// Whether this intake produced a buildable plan.
    pub fn is_ready(&self) -> bool {
        matches!(self, Intake::Ready { .. })
    }

    /// The outstanding clarifying questions for this turn, if any.
    pub fn questions(&self) -> &[String] {
        match self {
            Intake::NeedsClarification { questions, .. } => questions,
            _ => &[],
        }
    }

    /// The [`LeadEngineerResponse`] regardless of which variant this is. Always
    /// present so the UI can render the checklist and confidence on every screen.
    pub fn response(&self) -> &LeadEngineerResponse {
        match self {
            Intake::Ready { response, .. } => response,
            Intake::NeedsClarification { response, .. } => response,
            Intake::RecommendArchitect { response, .. } => response,
            Intake::TooComplex { response, .. } => response,
        }
    }

    /// Whether the honesty verdict permits a build attempt (`Proceed`). Returns
    /// `false` for `RecommendArchitect` and `TooComplex` regardless of plan.
    pub fn can_build(&self) -> bool {
        self.response().verdict.can_build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{ConfidenceScore, HonestyVerdict, LeadEngineerResponse};

    fn sample_plan() -> Plan {
        Plan {
            app_name: "budget".to_string(),
            summary: "a tiny budgeting app".to_string(),
            tasks: vec![PlanTask {
                role: "Implementer".to_string(),
                kind: TaskKind::Backend,
                description: "build the Expense entity".to_string(),
            }],
        }
    }

    fn minimal_response() -> LeadEngineerResponse {
        LeadEngineerResponse {
            checklist: vec![],
            confidence: ConfidenceScore::new(100),
            suggestions: vec![],
            verdict: HonestyVerdict::Proceed,
            questions: vec![],
        }
    }

    #[test]
    fn ready_exposes_plan_and_no_questions() {
        let intake = Intake::Ready {
            plan: sample_plan(),
            response: minimal_response(),
        };
        assert!(intake.is_ready());
        assert!(intake.plan().is_some());
        assert!(intake.questions().is_empty());
        assert!(intake.can_build());
    }

    #[test]
    fn needs_clarification_exposes_questions_and_no_plan() {
        let intake = Intake::NeedsClarification {
            questions: vec!["which currency?".to_string()],
            response: minimal_response(),
        };
        assert!(!intake.is_ready());
        assert!(intake.plan().is_none());
        assert_eq!(intake.questions(), &["which currency?".to_string()]);
    }

    #[test]
    fn recommend_architect_cannot_build_and_has_reason() {
        let reason = "needs real-time collaboration across shards".to_string();
        let intake = Intake::RecommendArchitect {
            reason: reason.clone(),
            response: LeadEngineerResponse {
                verdict: HonestyVerdict::RecommendArchitect { reason: reason.clone() },
                ..minimal_response()
            },
        };
        assert!(!intake.is_ready());
        assert!(!intake.can_build());
        assert!(intake.plan().is_none());
    }

    #[test]
    fn too_complex_cannot_build_and_has_reason() {
        let reason = "ML pipeline required, beyond CRUD scope".to_string();
        let intake = Intake::TooComplex {
            reason: reason.clone(),
            response: LeadEngineerResponse {
                verdict: HonestyVerdict::TooComplex { reason: reason.clone() },
                ..minimal_response()
            },
        };
        assert!(!intake.is_ready());
        assert!(!intake.can_build());
    }

    #[test]
    fn response_is_accessible_from_every_variant() {
        let r = minimal_response();
        let variants: Vec<Intake> = vec![
            Intake::Ready { plan: sample_plan(), response: r.clone() },
            Intake::NeedsClarification { questions: vec![], response: r.clone() },
            Intake::RecommendArchitect { reason: "x".into(), response: r.clone() },
            Intake::TooComplex { reason: "x".into(), response: r.clone() },
        ];
        for intake in &variants {
            assert_eq!(intake.response().confidence.value(), 100);
        }
    }
}
