//! The PO-mode project aggregate: the frozen onboarding document, the living
//! user stories (the source of truth), the refinement-session history, and the
//! lifecycle phase, in one persistable type.
//!
//! This is the top-level structure the consumer flow revolves around. It encodes
//! the rule the Product Owner set: after the onboarding document seeds the first
//! investigation it is FROZEN as read-only origin, and from then on the user
//! stories (and bug stories) are the source of truth, exactly as stories and bug
//! tickets are in real software development.
//!
//! The lifecycle is one primitive (the [`RefinementSession`]) alternating with
//! execution:
//!
//! ```text
//! Onboarding -> Refining (N) -> Executing -> Refining (post-build, N) -> ... -> Published
//! ```
//!
//! Each artifact on a [`Project`] is independently serializable, so the
//! persistence layer (`camerata-persistence`'s `ArtifactStore`) can record every
//! edit as a versioned revision in real time.

use serde::{Deserialize, Serialize};

use crate::form::IntakeForm;
use crate::refinement::{RefinementContext, RefinementSession};
use crate::story::UserStory;

// ─── lifecycle phase ─────────────────────────────────────────────────────────

/// Where a project is in its lifecycle. The phase gates which transitions are
/// legal (e.g. you cannot publish before you have executed at least once).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    /// The PO is filling out the onboarding document; no investigation yet.
    Onboarding,
    /// A refinement session is open (pre-build, mid-build, or post-build).
    Refining,
    /// A governed build is running.
    Executing,
    /// The app is live on the PO's own cloud.
    Published,
}

// ─── errors ──────────────────────────────────────────────────────────────────

/// Illegal lifecycle transitions. These are programmer errors / guard rails, not
/// user-facing copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleError {
    /// Tried to act on the onboarding document after it was frozen.
    OnboardingFrozen,
    /// Tried to enter execution without a converged, buildable session.
    NotReadyToExecute,
    /// Tried to publish before the app has ever been executed.
    NeverExecuted,
    /// Tried a transition that the current [`Phase`] does not allow.
    WrongPhase {
        /// The phase the project is actually in.
        actual: Phase,
    },
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LifecycleError::OnboardingFrozen => {
                write!(f, "the onboarding document is frozen and cannot be edited")
            }
            LifecycleError::NotReadyToExecute => write!(
                f,
                "cannot execute: the latest refinement session has not converged on a buildable spec"
            ),
            LifecycleError::NeverExecuted => {
                write!(f, "cannot publish: the app has never been executed")
            }
            LifecycleError::WrongPhase { actual } => {
                write!(f, "illegal transition from phase {actual:?}")
            }
        }
    }
}

impl std::error::Error for LifecycleError {}

// ─── the project ─────────────────────────────────────────────────────────────

/// The PO-mode project aggregate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    /// Stable project id.
    pub id: String,
    /// The onboarding document: the grand project plan the PO filled out first.
    /// It is the SEED for the first investigation and is frozen afterward.
    pub onboarding: IntakeForm,
    /// Whether the onboarding document has been frozen (read-only origin). Set
    /// once the first investigation turns it into user stories.
    pub onboarding_frozen: bool,
    /// The living user stories: the SOURCE OF TRUTH after freeze. Committed from
    /// converged refinement sessions.
    pub stories: Vec<UserStory>,
    /// The refinement-session history, in order. The last one is the active
    /// session (if any).
    pub sessions: Vec<RefinementSession>,
    /// How many times the project has been executed (built). Publishing requires
    /// at least one execution.
    pub executions: usize,
    /// The current lifecycle phase.
    pub phase: Phase,
    /// The project's opt-in consents for the shared design corpus (contribute this
    /// design / use historical designs). Defaults to opt-out.
    /// See [`crate::sharing`].
    #[serde(default)]
    pub sharing: crate::sharing::SharingPreferences,
}

impl Project {
    /// Start a new project from an onboarding document. Begins in
    /// [`Phase::Onboarding`].
    pub fn new(id: impl Into<String>, onboarding: IntakeForm) -> Self {
        Self {
            id: id.into(),
            onboarding,
            onboarding_frozen: false,
            stories: Vec::new(),
            sessions: Vec::new(),
            executions: 0,
            phase: Phase::Onboarding,
            sharing: crate::sharing::SharingPreferences::default(),
        }
    }

    /// Set the project's sharing consents (builder form), e.g. from the
    /// refinement screen's opt-in toggles.
    pub fn with_sharing(mut self, sharing: crate::sharing::SharingPreferences) -> Self {
        self.sharing = sharing;
        self
    }

    /// Freeze the onboarding document and seed the project with the stories the
    /// first investigation produced, then open the first pre-build refinement
    /// session over them. Moves to [`Phase::Refining`].
    ///
    /// Idempotent-ish: freezing twice is a [`LifecycleError::OnboardingFrozen`].
    pub fn seed_from_investigation(
        &mut self,
        session_id: impl Into<String>,
        stories: Vec<UserStory>,
    ) -> Result<&mut RefinementSession, LifecycleError> {
        if self.onboarding_frozen {
            return Err(LifecycleError::OnboardingFrozen);
        }
        self.onboarding_frozen = true;
        self.stories = stories.clone();
        let session = RefinementSession::pre_build(session_id, stories);
        self.sessions.push(session);
        self.phase = Phase::Refining;
        Ok(self.sessions.last_mut().expect("just pushed"))
    }

    /// Open a new refinement session in `context` over the current source-of-truth
    /// stories. Used for mid-build escalations and post-build bug sessions. Moves
    /// to [`Phase::Refining`].
    pub fn begin_session(
        &mut self,
        session_id: impl Into<String>,
        context: RefinementContext,
    ) -> &mut RefinementSession {
        let session =
            RefinementSession::open(session_id, context, self.stories.clone());
        self.sessions.push(session);
        self.phase = Phase::Refining;
        self.sessions.last_mut().expect("just pushed")
    }

    /// The active (most recent) refinement session, if any.
    pub fn active_session(&self) -> Option<&RefinementSession> {
        self.sessions.last()
    }

    /// Mutable access to the active session, if any.
    pub fn active_session_mut(&mut self) -> Option<&mut RefinementSession> {
        self.sessions.last_mut()
    }

    /// Commit the active session's stories into the project source of truth. Only
    /// legal once that session has converged. This is the moment the refined
    /// stories become the project's truth.
    pub fn commit_active_session(&mut self) -> Result<(), LifecycleError> {
        let session = self
            .sessions
            .last()
            .ok_or(LifecycleError::NotReadyToExecute)?;
        if !session.is_converged() {
            return Err(LifecycleError::NotReadyToExecute);
        }
        self.stories = session.stories.clone();
        Ok(())
    }

    /// Enter execution. Requires the active session to have converged on a
    /// buildable spec (committing it first). Moves to [`Phase::Executing`] and
    /// increments the execution count.
    pub fn enter_execution(&mut self) -> Result<(), LifecycleError> {
        let session = self
            .sessions
            .last()
            .ok_or(LifecycleError::NotReadyToExecute)?;
        if !session.is_converged() || !session.can_build() {
            return Err(LifecycleError::NotReadyToExecute);
        }
        self.stories = session.stories.clone();
        self.executions += 1;
        self.phase = Phase::Executing;
        Ok(())
    }

    /// Finish the current execution (the build completed). The project goes back
    /// to [`Phase::Refining`] conceptually (the PO QA-tests and may open a
    /// post-build session); here we simply leave it ready for the next move.
    pub fn finish_execution(&mut self) -> Result<(), LifecycleError> {
        if self.phase != Phase::Executing {
            return Err(LifecycleError::WrongPhase { actual: self.phase });
        }
        self.phase = Phase::Refining;
        Ok(())
    }

    /// Publish the app: draft to live. Requires at least one execution. Moves to
    /// [`Phase::Published`].
    pub fn publish(&mut self) -> Result<(), LifecycleError> {
        if self.executions == 0 {
            return Err(LifecycleError::NeverExecuted);
        }
        self.phase = Phase::Published;
        Ok(())
    }

    /// Whether the project is live.
    pub fn is_published(&self) -> bool {
        matches!(self.phase, Phase::Published)
    }

    /// The number of refinement sessions run so far.
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::ConfidenceScore;
    use crate::refinement::{BugReport, Escalation, RefinementContext, RefinementReview};
    use crate::story::UserStory;

    fn onboarding() -> IntakeForm {
        IntakeForm::sample_app()
    }

    fn story(id: &str) -> UserStory {
        UserStory::from_investigation(id, "T", "W", vec!["I can do a thing".to_string()])
    }

    fn ready_review() -> RefinementReview {
        RefinementReview {
            confidence: ConfidenceScore::new(90),
            ..Default::default()
        }
    }

    #[test]
    fn new_project_starts_in_onboarding() {
        let p = Project::new("p1", onboarding());
        assert_eq!(p.phase, Phase::Onboarding);
        assert!(!p.onboarding_frozen);
        assert!(p.stories.is_empty());
    }

    #[test]
    fn seeding_freezes_onboarding_and_opens_a_pre_build_session() {
        let mut p = Project::new("p1", onboarding());
        p.seed_from_investigation("s1", vec![story("a"), story("b")])
            .unwrap();
        assert!(p.onboarding_frozen);
        assert_eq!(p.phase, Phase::Refining);
        assert_eq!(p.stories.len(), 2);
        assert_eq!(p.session_count(), 1);
        assert_eq!(p.active_session().unwrap().context.label(), "pre_build");
    }

    #[test]
    fn cannot_seed_twice_after_freeze() {
        let mut p = Project::new("p1", onboarding());
        p.seed_from_investigation("s1", vec![story("a")]).unwrap();
        let err = p.seed_from_investigation("s2", vec![story("b")]).unwrap_err();
        assert_eq!(err, LifecycleError::OnboardingFrozen);
    }

    #[test]
    fn full_happy_path_onboarding_to_published() {
        let mut p = Project::new("p1", onboarding());
        // Investigation seeds stories + opens pre-build refinement.
        p.seed_from_investigation("s1", vec![story("a")]).unwrap();

        // Refine: AI review raises confidence, user converges.
        {
            let s = p.active_session_mut().unwrap();
            s.apply_review(ready_review());
            s.converge();
        }
        // Enter + finish the first execution.
        p.enter_execution().unwrap();
        assert_eq!(p.phase, Phase::Executing);
        assert_eq!(p.executions, 1);
        p.finish_execution().unwrap();
        assert_eq!(p.phase, Phase::Refining);

        // Publish.
        p.publish().unwrap();
        assert!(p.is_published());
    }

    #[test]
    fn cannot_execute_before_session_converges() {
        let mut p = Project::new("p1", onboarding());
        p.seed_from_investigation("s1", vec![story("a")]).unwrap();
        // Session has not converged yet.
        let err = p.enter_execution().unwrap_err();
        assert_eq!(err, LifecycleError::NotReadyToExecute);
    }

    #[test]
    fn cannot_execute_when_verdict_blocks_build() {
        use crate::engine::HonestyVerdict;
        let mut p = Project::new("p1", onboarding());
        p.seed_from_investigation("s1", vec![story("a")]).unwrap();
        {
            let s = p.active_session_mut().unwrap();
            let mut r = ready_review();
            r.verdict = HonestyVerdict::RecommendArchitect {
                reason: "needs a human".to_string(),
            };
            s.apply_review(r);
            s.converge();
        }
        // Converged but not buildable: blocked.
        let err = p.enter_execution().unwrap_err();
        assert_eq!(err, LifecycleError::NotReadyToExecute);
    }

    #[test]
    fn cannot_publish_before_any_execution() {
        let mut p = Project::new("p1", onboarding());
        p.seed_from_investigation("s1", vec![story("a")]).unwrap();
        let err = p.publish().unwrap_err();
        assert_eq!(err, LifecycleError::NeverExecuted);
    }

    #[test]
    fn mid_build_escalation_opens_a_session_then_resumes() {
        let mut p = Project::new("p1", onboarding());
        p.seed_from_investigation("s1", vec![story("a")]).unwrap();
        {
            let s = p.active_session_mut().unwrap();
            s.apply_review(ready_review());
            s.converge();
        }
        p.enter_execution().unwrap();
        assert_eq!(p.phase, Phase::Executing);

        // A builder escalates mid-build: open a mid-build session.
        let esc = Escalation::new("export task", "Spreadsheet or PDF?");
        p.begin_session("s2", RefinementContext::MidBuild { escalation: esc });
        assert_eq!(p.phase, Phase::Refining);
        assert_eq!(p.active_session().unwrap().context.label(), "mid_build");

        // Resolve it, then resume execution.
        {
            let s = p.active_session_mut().unwrap();
            s.apply_review(ready_review());
            s.converge();
        }
        p.enter_execution().unwrap();
        assert_eq!(p.executions, 2);
    }

    #[test]
    fn post_build_bug_session_then_re_execute() {
        let mut p = Project::new("p1", onboarding());
        p.seed_from_investigation("s1", vec![story("a")]).unwrap();
        {
            let s = p.active_session_mut().unwrap();
            s.apply_review(ready_review());
            s.converge();
        }
        p.enter_execution().unwrap();
        p.finish_execution().unwrap();

        // QA found a bug: open a post-build session.
        let bug = BugReport::new("List", "clicked add", "a row", "nothing");
        p.begin_session(
            "s2",
            RefinementContext::PostBuild { bugs: vec![bug] },
        );
        // The bug became a bug story in the session.
        assert!(p
            .active_session()
            .unwrap()
            .stories
            .iter()
            .any(|s| s.origin == crate::story::StoryOrigin::BugReport));
        {
            let s = p.active_session_mut().unwrap();
            s.apply_review(ready_review());
            s.converge();
        }
        p.enter_execution().unwrap();
        assert_eq!(p.executions, 2);
        p.finish_execution().unwrap();
        p.publish().unwrap();
        assert!(p.is_published());
    }

    #[test]
    fn commit_active_session_makes_stories_the_source_of_truth() {
        let mut p = Project::new("p1", onboarding());
        p.seed_from_investigation("s1", vec![story("a")]).unwrap();
        {
            let s = p.active_session_mut().unwrap();
            // User adds a story during refinement.
            s.add_story(story("b"));
            s.apply_review(ready_review());
            s.converge();
        }
        p.commit_active_session().unwrap();
        // The project's source of truth now reflects the session's edits.
        assert_eq!(p.stories.len(), 2);
    }

    #[test]
    fn finish_execution_only_legal_while_executing() {
        let mut p = Project::new("p1", onboarding());
        let err = p.finish_execution().unwrap_err();
        assert_eq!(err, LifecycleError::WrongPhase { actual: Phase::Onboarding });
    }

    #[test]
    fn project_round_trips_json() {
        let mut p = Project::new("p1", onboarding());
        p.seed_from_investigation("s1", vec![story("a")]).unwrap();
        let json = serde_json::to_string(&p).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back, p);
    }
}
