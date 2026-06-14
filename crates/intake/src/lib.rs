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

pub use clarify::{
    AnswerSource, ClarifyDriver, ClarifyError, ClarifyOutcome, SequentialAnswerSource,
    StubAnswerSource,
};
pub use engine::{ClaudeLeadEngineer, LeadEngineer, LeadEngineerError, StubLeadEngineer};
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

use serde::{Deserialize, Serialize};

/// The result of running a [`LeadEngineer`] over an [`IntakeForm`].
///
/// This is the clarify-loop state machine in one type. A lead engineer either
/// has enough to plan ([`Intake::Ready`]) or needs the Product Owner to answer
/// questions first ([`Intake::NeedsClarification`]). V1's `po-demo` drives the
/// `Ready` arm end to end; feeding `NeedsClarification` back to the PO and
/// re-evaluating is the multi-turn clarify loop (documented as remaining work).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Intake {
    /// The lead engineer understood the form well enough to produce a plan.
    Ready(Plan),
    /// The lead engineer needs answers before it will commit to a plan. Each
    /// string is one question for the Product Owner.
    NeedsClarification(Vec<String>),
}

impl Intake {
    /// The plan, if this intake is [`Intake::Ready`]; otherwise `None`.
    pub fn plan(&self) -> Option<&Plan> {
        match self {
            Intake::Ready(plan) => Some(plan),
            Intake::NeedsClarification(_) => None,
        }
    }

    /// Whether this intake produced a buildable plan.
    pub fn is_ready(&self) -> bool {
        matches!(self, Intake::Ready(_))
    }

    /// The outstanding clarifying questions, if any.
    pub fn questions(&self) -> &[String] {
        match self {
            Intake::NeedsClarification(qs) => qs,
            Intake::Ready(_) => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn ready_exposes_plan_and_no_questions() {
        let intake = Intake::Ready(sample_plan());
        assert!(intake.is_ready());
        assert!(intake.plan().is_some());
        assert!(intake.questions().is_empty());
    }

    #[test]
    fn needs_clarification_exposes_questions_and_no_plan() {
        let intake = Intake::NeedsClarification(vec!["which currency?".to_string()]);
        assert!(!intake.is_ready());
        assert!(intake.plan().is_none());
        assert_eq!(intake.questions(), &["which currency?".to_string()]);
    }
}
