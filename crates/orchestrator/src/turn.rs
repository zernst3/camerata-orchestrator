//! The orchestrator TURN: `handle_turn` drives one change-request round-trip for an
//! EXISTING scaffolded project, end to end.
//!
//! See `docs/plans/2026-07-09_product-owner-head-vibe-mode.md`, the
//! "Architect-orchestrator design (decided 2026-07-10)" section. This module is the
//! crate's own doc's promised "later phase": "The orchestrator LOOP that calls
//! [`crate::classify`] on every proposed action, drives the fleet, and records the
//! outcome."
//!
//! # The seam: traits, not concrete types
//! [`TurnLlm`] and [`TurnExecutor`] are small, object-safe traits. Tests inject fakes
//! (see this module's tests). PRODUCTION wiring (in `camerata-server`) implements
//! them over the REAL LLM (`camerata_llm::Llm` via `AppState::llm()`) and the REAL
//! governed execution seam (`start_governed_run`) — never a placeholder. If either
//! seam is unavailable in production (no live LLM/key, no resolvable local repo),
//! the real implementation returns an HONEST `Err`, which `handle_turn` propagates
//! unchanged: this module never fabricates a result when a real one couldn't be
//! produced.
//!
//! [`DecisionRecorder`] is ALSO a trait rather than a direct call into
//! `camerata_persistence::OrchestratorDecisionLog`, for the same reason
//! `camerata-persistence` stores `class`/`confidence` as plain strings instead of
//! importing this crate's enums (see `orchestrator_decision.rs`'s module doc): it
//! keeps `camerata-orchestrator-core` decoupled from the persistence crate. The
//! production impl wraps `AppState::record_orchestrator_decision`.
//!
//! # Class D (clarification) is decided at intake, not by `classify`
//! Per this crate's root module doc, [`crate::classify`] never returns a fourth
//! "genuine ambiguity" class — that judgment belongs to intake. Here, the REAL LLM
//! itself makes that call when interpreting the message: [`Interpretation::Clarify`]
//! is how it says "this is load-bearing ambiguity, ask before proposing anything."
//! `handle_turn` takes that at face value and returns
//! [`TurnOutcome::NeedsClarification`] without ever calling `classify` or the
//! executor — nothing is decided, so nothing is recorded.

use crate::{classify, Action, ClassifyCtx, Confidence, ContentSignal, DecisionClass};

// ─── the LLM seam ───────────────────────────────────────────────────────────────

/// One file/area the proposed change will touch, as the LLM named it.
///
/// Mirrors [`Action::Write`]'s shape loosely enough that [`classify_change`] can
/// build an [`Action`] from each entry without this module re-deriving any
/// effect-signature logic — `classify` (and the real deny-before-execute gate it is
/// grounded in) stays the SINGLE place that decision lives.
#[derive(Debug, Clone, PartialEq)]
pub struct TouchedArea {
    pub path: String,
    /// Whether this touch is known to add a new dependency (only meaningful for a
    /// `Cargo.toml` touch). `None` when unknown — [`crate::classify`]'s existing
    /// conservative default (an unknown-diff `Cargo.toml` write is Irreversible)
    /// still applies untouched.
    pub adds_dependency: Option<bool>,
}

impl TouchedArea {
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            adds_dependency: None,
        }
    }

    pub fn adding_dependency(path: impl Into<String>, adds: bool) -> Self {
        Self {
            path: path.into(),
            adds_dependency: Some(adds),
        }
    }
}

/// The LLM's structural interpretation of a change-request message: a title,
/// description, and the concrete areas it will touch (so [`classify_change`] can
/// ground the decision class in something structural, never in the model's own
/// self-reported confidence — see the crate root's module doc on why).
#[derive(Debug, Clone, PartialEq)]
pub struct ProposedChange {
    pub title: String,
    pub description: String,
    pub touches: Vec<TouchedArea>,
    /// An assumption the LLM declared instead of asking a clarifying question (the
    /// plan doc's "assume-and-declare into an assumptions ledger" pattern). `None`
    /// when nothing was assumed.
    pub assumption: Option<String>,
}

/// What the LLM decided about a change-request message this turn.
#[derive(Debug, Clone, PartialEq)]
pub enum Interpretation {
    /// A structured change to route + (maybe) execute.
    Propose(ProposedChange),
    /// Genuine, load-bearing intake ambiguity (Class D) — ask before proposing
    /// anything. Capped at a handful of customer-phrased questions per the plan
    /// doc ("Cap ~3 batched, customer-phrased questions per round"); this module
    /// does not enforce the cap itself (that is a prompting concern for the real
    /// LLM impl), it just carries whatever the LLM returned.
    Clarify(Vec<String>),
}

/// Interprets a change-request message into a structural [`Interpretation`], given
/// the project's living spec as context. Object-safe (`&dyn TurnLlm`) so production
/// can pass the real model while tests inject a fake.
#[async_trait::async_trait]
pub trait TurnLlm: Send + Sync {
    async fn interpret(
        &self,
        spec: &crate::spec::LivingSpec,
        message: &str,
    ) -> anyhow::Result<Interpretation>;
}

// ─── the execution seam ─────────────────────────────────────────────────────────

/// What actually happened when a Class A/B change was driven through the real
/// governed execution seam.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionSummary {
    /// The identifier of the underlying governed run (whatever the executor's
    /// concrete backend calls it — a `start_governed_run` run id in production).
    pub run_id: String,
    /// Human-readable summary of what the run did / concluded.
    pub summary: String,
}

/// Drives the REAL governed execution seam for a proposed change. Object-safe so
/// production can wrap `start_governed_run` while tests inject a fake that never
/// touches a filesystem or spawns a fleet.
#[async_trait::async_trait]
pub trait TurnExecutor: Send + Sync {
    async fn execute(&self, change: &ProposedChange) -> anyhow::Result<ExecutionSummary>;
}

// ─── decision recording ─────────────────────────────────────────────────────────

/// One decision to record, in the vocabulary [`crate::classify`] produces. Kept as
/// crate-native enums (not persistence's plain strings) — the [`DecisionRecorder`]
/// impl converts at its own boundary, mirroring how `camerata-persistence` converts
/// at ITS boundary (see this module's doc).
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedDecisionInput {
    pub run_id: String,
    pub class: DecisionClass,
    pub confidence: Confidence,
    pub chosen: String,
    pub alternatives: Vec<String>,
    pub assumption: Option<String>,
}

/// Records one autonomous decision, returning the assigned row id when a store is
/// attached (`None` is a legitimate, fail-soft outcome — see
/// `AppState::record_orchestrator_decision`'s contract in `camerata-server`; a
/// missing decision store never blocks the turn itself).
#[async_trait::async_trait]
pub trait DecisionRecorder: Send + Sync {
    async fn record(&self, decision: RecordedDecisionInput) -> Option<i64>;
}

/// A recorded decision, echoed back on [`TurnOutcome`] so a caller can show "what it
/// decided on its own" without this crate depending on the persistence row shape.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionSummary {
    pub class: DecisionClass,
    pub confidence: Confidence,
    /// The decision store's row id, when a store was attached and the write
    /// succeeded.
    pub id: Option<i64>,
}

// ─── the turn outcome ───────────────────────────────────────────────────────────

/// What one orchestrator turn concluded.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    /// Class A/B: driven through the real governed execution seam and (on success)
    /// applied. The living spec has already been updated by the time this is
    /// returned.
    Applied {
        summary: String,
        decision: DecisionSummary,
    },
    /// Class C (irreversible / high blast radius): NOT executed. The dial can never
    /// override this — see the plan doc's classification table.
    NeedsApproval {
        consequence: String,
        decision: DecisionSummary,
    },
    /// Class D (genuine, load-bearing intake ambiguity): nothing was classified or
    /// executed; the LLM itself decided the message needs clarification first.
    NeedsClarification { questions: Vec<String> },
}

/// The seams `handle_turn` drives. Borrowed rather than owned so callers (tests and
/// production alike) control the concrete types' lifetimes/ownership.
pub struct TurnDeps<'a> {
    pub llm: &'a dyn TurnLlm,
    pub executor: &'a dyn TurnExecutor,
    pub recorder: &'a dyn DecisionRecorder,
}

/// Run one orchestrator turn against an EXISTING scaffolded project's repo:
///
/// 1. Load the living spec (`SPEC.md` in `repo_dir`) as context.
/// 2. Call the LLM to interpret `message` into a structural [`Interpretation`].
/// 3. `Clarify` -> return [`TurnOutcome::NeedsClarification`] immediately (nothing
///    classified, nothing recorded — genuine intake ambiguity is decided BEFORE an
///    action is ever proposed; see this module's doc).
/// 4. `Propose(change)` -> classify every touched area via [`crate::classify`],
///    combine to one overall (worst-case) [`crate::Classification`], and record the
///    decision (best-effort: `recorder` may be a no-op store; that never blocks the
///    turn).
/// 5. Route on the combined class:
///    - `Irreversible` -> [`TurnOutcome::NeedsApproval`], executor is NEVER called
///      (the dial cannot override Class C).
///    - `MechanicallyVerified` / `PreviewReversible` -> drive the executor; on
///      success, update + persist the living spec (change-log entry, plus an
///      assumption-ledger entry if the LLM declared one) and return
///      [`TurnOutcome::Applied`].
///
/// An LLM or executor failure is an HONEST `Err` propagated to the caller (never
/// papered over as a `TurnOutcome` variant) — the no-placeholder rule means a turn
/// that could not actually be carried out must fail loudly, not report a fake
/// success.
pub async fn handle_turn(
    deps: &TurnDeps<'_>,
    repo_dir: &std::path::Path,
    turn_id: &str,
    message: &str,
) -> anyhow::Result<TurnOutcome> {
    let spec = crate::spec::read_or_init(repo_dir)?;

    let interpretation = deps.llm.interpret(&spec, message).await?;
    let change = match interpretation {
        Interpretation::Clarify(questions) => {
            return Ok(TurnOutcome::NeedsClarification { questions })
        }
        Interpretation::Propose(change) => change,
    };

    let classification = classify_change(&change);
    let decision_id = deps
        .recorder
        .record(RecordedDecisionInput {
            run_id: turn_id.to_string(),
            class: classification.class,
            confidence: classification.confidence,
            chosen: change.title.clone(),
            alternatives: Vec::new(),
            assumption: change.assumption.clone(),
        })
        .await;
    let decision = DecisionSummary {
        class: classification.class,
        confidence: classification.confidence,
        id: decision_id,
    };

    match classification.class {
        DecisionClass::Irreversible => Ok(TurnOutcome::NeedsApproval {
            consequence: describe_consequence(&change),
            decision,
        }),
        DecisionClass::MechanicallyVerified | DecisionClass::PreviewReversible => {
            let exec = deps.executor.execute(&change).await?;
            let mut updated_spec = spec.with_change(exec.summary.clone(), chrono::Utc::now());
            if let Some(assumption) = &change.assumption {
                updated_spec = updated_spec.with_assumption(assumption.clone(), chrono::Utc::now());
            }
            crate::spec::write(repo_dir, &updated_spec)?;
            Ok(TurnOutcome::Applied {
                summary: exec.summary,
                decision,
            })
        }
    }
}

/// Classify a proposed change: build one [`Action::Write`] per touched area, run
/// [`crate::classify`] on each, and combine to the WORST-CASE overall result — the
/// highest-risk class wins (an Irreversible touch anywhere makes the whole change
/// Irreversible), and the lowest confidence wins (one shaky touch drags the whole
/// change's confidence down). A change naming NO touched areas at all cannot be
/// verified safe by anything, so it is conservatively treated as Irreversible
/// (Medium confidence) rather than silently auto-executed.
fn classify_change(change: &ProposedChange) -> crate::Classification {
    if change.touches.is_empty() {
        return crate::Classification {
            class: DecisionClass::Irreversible,
            confidence: Confidence::Medium,
        };
    }
    let ctx = ClassifyCtx::default();
    let mut worst_class = DecisionClass::MechanicallyVerified;
    let mut worst_confidence = Confidence::High;
    for touch in &change.touches {
        let action = Action::Write {
            path: touch.path.clone(),
            content_signal: ContentSignal {
                content: None,
                adds_dependency: touch.adds_dependency,
            },
        };
        let result = classify(&action, &ctx);
        if class_rank(result.class) > class_rank(worst_class) {
            worst_class = result.class;
        }
        if confidence_rank(result.confidence) < confidence_rank(worst_confidence) {
            worst_confidence = result.confidence;
        }
    }
    crate::Classification {
        class: worst_class,
        confidence: worst_confidence,
    }
}

fn class_rank(c: DecisionClass) -> u8 {
    match c {
        DecisionClass::MechanicallyVerified => 0,
        DecisionClass::PreviewReversible => 1,
        DecisionClass::Irreversible => 2,
    }
}

fn confidence_rank(c: Confidence) -> u8 {
    match c {
        Confidence::Low => 0,
        Confidence::Medium => 1,
        Confidence::High => 2,
    }
}

/// Human-readable explanation of why a change was routed to `NeedsApproval`, for
/// the human-review checkpoint (the plan doc's "consequence-first approval copy").
/// Pure text assembly — no re-classification here (that already happened; this just
/// narrates it).
fn describe_consequence(change: &ProposedChange) -> String {
    if change.touches.is_empty() {
        return format!(
            "\"{}\" does not name any specific file or area it will touch, so its effect \
             cannot be verified safe by the compiler, the gate, or the live preview — \
             treating it as irreversible until a human approves.",
            change.title
        );
    }
    let paths = change
        .touches
        .iter()
        .map(|t| t.path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "\"{}\" touches: {paths}. At least one of these is irreversible / high blast \
         radius (a migration, terraform, a secret file, a Cargo dependency change, or a \
         destructive statement) — no automatic backstop (compiler, gate, or live preview) \
         judges whether this is the right call, so it requires explicit human approval \
         before it runs.",
        change.title
    )
}

// ─────────────────────────────────────────────────────────────────────────────────
// Tests (ORCH-NEW-PATH-TESTS-1)
// ─────────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    // ── classify_change ──────────────────────────────────────────────────────

    fn change_with_touches(touches: Vec<TouchedArea>) -> ProposedChange {
        ProposedChange {
            title: "test change".to_string(),
            description: "test description".to_string(),
            touches,
            assumption: None,
        }
    }

    #[test]
    fn classify_change_with_no_touches_is_conservatively_irreversible() {
        let change = change_with_touches(vec![]);
        let result = classify_change(&change);
        assert_eq!(result.class, DecisionClass::Irreversible);
        assert_eq!(result.confidence, Confidence::Medium);
    }

    #[test]
    fn classify_change_single_server_rs_touch_is_mechanically_verified() {
        let change = change_with_touches(vec![TouchedArea::new("src/server.rs")]);
        let result = classify_change(&change);
        assert_eq!(result.class, DecisionClass::MechanicallyVerified);
    }

    #[test]
    fn classify_change_single_view_touch_is_preview_reversible() {
        let change = change_with_touches(vec![TouchedArea::new("src/pages/home.rs")]);
        let result = classify_change(&change);
        assert_eq!(result.class, DecisionClass::PreviewReversible);
    }

    #[test]
    fn classify_change_mixed_touches_picks_worst_case_class() {
        // One safe .rs touch + one migrations touch -> overall Irreversible.
        let change = change_with_touches(vec![
            TouchedArea::new("src/server.rs"),
            TouchedArea::new("migrations/0002_add_col.sql"),
        ]);
        let result = classify_change(&change);
        assert_eq!(result.class, DecisionClass::Irreversible);
    }

    #[test]
    fn classify_change_mixed_class_and_preview_picks_preview() {
        let change = change_with_touches(vec![
            TouchedArea::new("src/server.rs"),        // MechanicallyVerified
            TouchedArea::new("src/components/nav.rs"), // PreviewReversible
        ]);
        let result = classify_change(&change);
        assert_eq!(result.class, DecisionClass::PreviewReversible);
    }

    #[test]
    fn classify_change_unknown_cargo_toml_dependency_is_irreversible() {
        let change = change_with_touches(vec![TouchedArea::new("Cargo.toml")]);
        let result = classify_change(&change);
        assert_eq!(result.class, DecisionClass::Irreversible);
    }

    #[test]
    fn classify_change_cargo_toml_with_dependency_cleared_is_not_irreversible() {
        let change = change_with_touches(vec![TouchedArea::adding_dependency(
            "Cargo.toml",
            false,
        )]);
        let result = classify_change(&change);
        assert_eq!(result.class, DecisionClass::PreviewReversible);
    }

    // ── fakes for handle_turn ────────────────────────────────────────────────

    struct FakeLlm {
        result: Mutex<Option<anyhow::Result<Interpretation>>>,
    }

    impl FakeLlm {
        fn propose(change: ProposedChange) -> Self {
            Self {
                result: Mutex::new(Some(Ok(Interpretation::Propose(change)))),
            }
        }
        fn clarify(questions: Vec<&str>) -> Self {
            Self {
                result: Mutex::new(Some(Ok(Interpretation::Clarify(
                    questions.into_iter().map(String::from).collect(),
                )))),
            }
        }
        fn erroring(message: &'static str) -> Self {
            Self {
                result: Mutex::new(Some(Err(anyhow::anyhow!(message)))),
            }
        }
    }

    #[async_trait::async_trait]
    impl TurnLlm for FakeLlm {
        async fn interpret(
            &self,
            _spec: &crate::spec::LivingSpec,
            _message: &str,
        ) -> anyhow::Result<Interpretation> {
            self.result
                .lock()
                .expect("mutex")
                .take()
                .expect("interpret called more than once in this test")
        }
    }

    #[derive(Default)]
    struct FakeExecutor {
        calls: AtomicUsize,
        fail: bool,
    }

    impl FakeExecutor {
        fn succeeding() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail: false,
            }
        }
        fn failing() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                fail: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl TurnExecutor for FakeExecutor {
        async fn execute(&self, change: &ProposedChange) -> anyhow::Result<ExecutionSummary> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                anyhow::bail!("execution failed honestly");
            }
            Ok(ExecutionSummary {
                run_id: "run-1".to_string(),
                summary: format!("Implemented: {}", change.title),
            })
        }
    }

    #[derive(Default)]
    struct FakeRecorder {
        recorded: Mutex<Vec<RecordedDecisionInput>>,
    }

    #[async_trait::async_trait]
    impl DecisionRecorder for FakeRecorder {
        async fn record(&self, decision: RecordedDecisionInput) -> Option<i64> {
            let mut guard = self.recorded.lock().expect("mutex");
            guard.push(decision);
            Some(guard.len() as i64)
        }
    }

    struct NullRecorder;
    #[async_trait::async_trait]
    impl DecisionRecorder for NullRecorder {
        async fn record(&self, _decision: RecordedDecisionInput) -> Option<i64> {
            None
        }
    }

    // ── handle_turn: Clarify path ────────────────────────────────────────────

    #[tokio::test]
    async fn handle_turn_clarify_never_classifies_or_executes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let llm = FakeLlm::clarify(vec!["Does this need end-user logins?"]);
        let executor = FakeExecutor::succeeding();
        let recorder = FakeRecorder::default();
        let deps = TurnDeps {
            llm: &llm,
            executor: &executor,
            recorder: &recorder,
        };

        let outcome = handle_turn(&deps, dir.path(), "turn-1", "build me a thing")
            .await
            .expect("handle_turn");

        match outcome {
            TurnOutcome::NeedsClarification { questions } => {
                assert_eq!(questions, vec!["Does this need end-user logins?".to_string()]);
            }
            other => panic!("expected NeedsClarification, got {other:?}"),
        }
        assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
        assert!(recorder.recorded.lock().expect("mutex").is_empty());
    }

    // ── handle_turn: Applied path (Class A/B) ───────────────────────────────

    #[tokio::test]
    async fn handle_turn_applied_executes_and_updates_spec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let change = ProposedChange {
            title: "Fix $0 input bug".to_string(),
            description: "Guard against a zero amount crashing the form.".to_string(),
            touches: vec![TouchedArea::new("src/pages/entry_form.rs")],
            assumption: None,
        };
        let llm = FakeLlm::propose(change);
        let executor = FakeExecutor::succeeding();
        let recorder = FakeRecorder::default();
        let deps = TurnDeps {
            llm: &llm,
            executor: &executor,
            recorder: &recorder,
        };

        let outcome = handle_turn(&deps, dir.path(), "turn-2", "fix the $0 bug")
            .await
            .expect("handle_turn");

        match outcome {
            TurnOutcome::Applied { summary, decision } => {
                assert_eq!(summary, "Implemented: Fix $0 input bug");
                assert_eq!(decision.class, DecisionClass::PreviewReversible);
                assert_eq!(decision.id, Some(1));
            }
            other => panic!("expected Applied, got {other:?}"),
        }
        assert_eq!(executor.calls.load(Ordering::SeqCst), 1);
        assert_eq!(recorder.recorded.lock().expect("mutex").len(), 1);

        // The living spec was actually updated on disk.
        let spec = crate::spec::read_or_init(dir.path()).expect("read spec");
        assert_eq!(spec.changes.len(), 1);
        assert_eq!(spec.changes[0].summary, "Implemented: Fix $0 input bug");
    }

    #[tokio::test]
    async fn handle_turn_applied_records_declared_assumption_in_spec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let change = ProposedChange {
            title: "Add a currency field".to_string(),
            description: "".to_string(),
            touches: vec![TouchedArea::new("src/pages/settings.rs")],
            assumption: Some("Assumed USD as the default currency.".to_string()),
        };
        let llm = FakeLlm::propose(change);
        let executor = FakeExecutor::succeeding();
        let recorder = NullRecorder;
        let deps = TurnDeps {
            llm: &llm,
            executor: &executor,
            recorder: &recorder,
        };

        handle_turn(&deps, dir.path(), "turn-3", "add currency support")
            .await
            .expect("handle_turn");

        let spec = crate::spec::read_or_init(dir.path()).expect("read spec");
        assert_eq!(spec.assumptions.len(), 1);
        assert_eq!(spec.assumptions[0].text, "Assumed USD as the default currency.");
    }

    #[tokio::test]
    async fn handle_turn_applied_with_no_decision_store_still_applies() {
        // DecisionRecorder returning None (fail-soft, mirrors
        // AppState::record_orchestrator_decision) never blocks the turn.
        let dir = tempfile::tempdir().expect("tempdir");
        let change = change_with_touches(vec![TouchedArea::new("src/lib.rs")]);
        let llm = FakeLlm::propose(change);
        let executor = FakeExecutor::succeeding();
        let recorder = NullRecorder;
        let deps = TurnDeps {
            llm: &llm,
            executor: &executor,
            recorder: &recorder,
        };

        let outcome = handle_turn(&deps, dir.path(), "turn-4", "do something")
            .await
            .expect("handle_turn");
        match outcome {
            TurnOutcome::Applied { decision, .. } => assert_eq!(decision.id, None),
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    // ── handle_turn: NeedsApproval path (Class C) ───────────────────────────

    #[tokio::test]
    async fn handle_turn_irreversible_never_calls_executor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let change = change_with_touches(vec![TouchedArea::new("migrations/0003_drop.sql")]);
        let llm = FakeLlm::propose(change);
        let executor = FakeExecutor::succeeding();
        let recorder = FakeRecorder::default();
        let deps = TurnDeps {
            llm: &llm,
            executor: &executor,
            recorder: &recorder,
        };

        let outcome = handle_turn(&deps, dir.path(), "turn-5", "drop the old table")
            .await
            .expect("handle_turn");

        match outcome {
            TurnOutcome::NeedsApproval { consequence, decision } => {
                assert_eq!(decision.class, DecisionClass::Irreversible);
                assert!(consequence.contains("migrations/0003_drop.sql"));
            }
            other => panic!("expected NeedsApproval, got {other:?}"),
        }
        assert_eq!(executor.calls.load(Ordering::SeqCst), 0);
        // The decision IS still recorded even though nothing executed (the audit
        // trail should show every autonomous decision, not just applied ones).
        assert_eq!(recorder.recorded.lock().expect("mutex").len(), 1);
    }

    #[tokio::test]
    async fn handle_turn_irreversible_does_not_touch_the_spec() {
        let dir = tempfile::tempdir().expect("tempdir");
        let change = change_with_touches(vec![TouchedArea::new("terraform/main.tf")]);
        let llm = FakeLlm::propose(change);
        let executor = FakeExecutor::succeeding();
        let recorder = NullRecorder;
        let deps = TurnDeps {
            llm: &llm,
            executor: &executor,
            recorder: &recorder,
        };

        handle_turn(&deps, dir.path(), "turn-6", "reprovision infra")
            .await
            .expect("handle_turn");

        assert!(!crate::spec::spec_path(dir.path()).exists());
    }

    // ── handle_turn: honest error propagation ───────────────────────────────

    #[tokio::test]
    async fn handle_turn_propagates_llm_failure_honestly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let llm = FakeLlm::erroring("live LLM required; set the model/key");
        let executor = FakeExecutor::succeeding();
        let recorder = NullRecorder;
        let deps = TurnDeps {
            llm: &llm,
            executor: &executor,
            recorder: &recorder,
        };

        let err = handle_turn(&deps, dir.path(), "turn-7", "anything")
            .await
            .expect_err("must fail honestly, not fabricate a TurnOutcome");
        assert!(err.to_string().contains("live LLM required"));
    }

    #[tokio::test]
    async fn handle_turn_propagates_executor_failure_honestly() {
        let dir = tempfile::tempdir().expect("tempdir");
        let change = change_with_touches(vec![TouchedArea::new("src/lib.rs")]);
        let llm = FakeLlm::propose(change);
        let executor = FakeExecutor::failing();
        let recorder = NullRecorder;
        let deps = TurnDeps {
            llm: &llm,
            executor: &executor,
            recorder: &recorder,
        };

        let err = handle_turn(&deps, dir.path(), "turn-8", "anything")
            .await
            .expect_err("must fail honestly");
        assert!(err.to_string().contains("execution failed honestly"));

        // A failed execution must not silently write a stale/fake spec update.
        assert!(!crate::spec::spec_path(dir.path()).exists());
    }
}
