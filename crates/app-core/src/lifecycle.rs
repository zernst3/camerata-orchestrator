//! The Unit-of-Work governed-development lifecycle state machine (Pillar 2).
//!
//! A story moves through a fixed sequence of dev-side lifecycle stages, and the
//! whole point of Camerata is that the sequence is ENFORCED: a story cannot reach
//! `Development` until its decisions are approved (the no-code-first gate), and it
//! cannot be `SignedOff` until QA has happened. This module is the pure, clock-free,
//! I/O-free core of that enforcement — every transition is a total function over the
//! current stage plus the relevant precondition, returning either the next stage or a
//! typed [`TransitionError`] explaining exactly why the move is illegal.
//!
//! The persistence layer ([`crate::uow::UowStore`]) and the HTTP handlers
//! ([`crate::lib`]) are thin wrappers around the functions here. Keeping the logic
//! pure means the lifecycle rules are exhaustively unit-testable without a database, a
//! gate, or a clock (RUST-PURE-STATE-TRANSITIONS-1).
//!
//! # The stages
//!
//! ```text
//!   Intake ──→ Investigating ──→ DecisionsApproved ──→ Development ──→ AwaitingQa ──→ SignedOff
//!     │              │                   │                  │              │
//!  begin_         decisions_         start_              run_           sign_
//! investigation   approved          development          complete       off
//! ```
//!
//! Each arrow is a named transition with a precondition:
//!
//! - **Intake → Investigating** ([`UowStage::begin_investigation`]): always allowed;
//!   moving a freshly-intaken story into active investigation.
//! - **Investigating → DecisionsApproved** ([`UowStage::approve_decisions`]): allowed
//!   ONLY when the supplied decision records pass
//!   [`camerata_worktracker::investigation::decisions_approved_for_development`]. This
//!   is the structural gate: at least one decision exists and every decision is
//!   `Approved`.
//! - **DecisionsApproved → Development** ([`UowStage::start_development`]): allowed
//!   only from `DecisionsApproved`. This is what the governed run start calls; it
//!   re-checks the decision gate as a belt-and-suspenders guard so a stale stage value
//!   can never let unreviewed code through.
//! - **Development → AwaitingQa** ([`UowStage::finish_development`]): the run reached
//!   its terminal/gating stage; the work is ready for QA.
//! - **AwaitingQa → SignedOff** ([`UowStage::sign_off`]): the architect's explicit,
//!   never-automatic gate after reviewing the provenance.
//!
//! Backward / corrective transitions are intentionally NOT modeled here yet (e.g.
//! sending a story back from `DecisionsApproved` to `Investigating` when a decision is
//! later rejected). Those are routed to the decision doc as a follow-up so the forward
//! happy-path lands first without speculative surface.

use serde::{Deserialize, Serialize};

use camerata_worktracker::investigation::{
    decisions_approved_for_development, DecisionRecord,
};

/// The dev-side lifecycle stage of a Unit of Work.
///
/// This is the SECOND status carried on a UoW, orthogonal to (but richer than) the
/// coarse [`crate::uow::DevStatus`] (New / InProgress / Done). `DevStatus` is the
/// at-a-glance badge; `UowStage` is the precise governed-development position used to
/// enforce the no-code-first gate and the QA gate.
///
/// The default is [`UowStage::Intake`]: a story whose UoW has just been created has
/// not begun investigation.
#[derive(Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum UowStage {
    /// The story has been adopted but dev work has not begun. Starting state.
    #[default]
    Intake,
    /// The AI agent is investigating: writing the investigation note and surfacing
    /// the structured decisions that must be resolved before code is written.
    Investigating,
    /// Every decision the investigation surfaced has been approved by the architect.
    /// This is the gate state: development may now start.
    DecisionsApproved,
    /// A governed run is (or has been) underway: agents writing code under the gate.
    Development,
    /// Development finished; the produced diff + gate provenance are awaiting the
    /// architect's QA review and sign-off.
    AwaitingQa,
    /// The architect has explicitly signed the work off. Terminal state.
    SignedOff,
}

impl UowStage {
    /// A short, stable display label for the UI and history entries.
    pub fn label(self) -> &'static str {
        match self {
            UowStage::Intake => "Intake",
            UowStage::Investigating => "Investigating",
            UowStage::DecisionsApproved => "Decisions approved",
            UowStage::Development => "Development",
            UowStage::AwaitingQa => "Awaiting QA",
            UowStage::SignedOff => "Signed off",
        }
    }

    /// The stable snake_case wire string (matches the serde representation).
    pub fn wire_str(self) -> &'static str {
        match self {
            UowStage::Intake => "intake",
            UowStage::Investigating => "investigating",
            UowStage::DecisionsApproved => "decisions_approved",
            UowStage::Development => "development",
            UowStage::AwaitingQa => "awaiting_qa",
            UowStage::SignedOff => "signed_off",
        }
    }

    /// Parse from the wire string. Returns `None` for unknown values.
    pub fn from_wire(s: &str) -> Option<Self> {
        match s {
            "intake" => Some(UowStage::Intake),
            "investigating" => Some(UowStage::Investigating),
            "decisions_approved" => Some(UowStage::DecisionsApproved),
            "development" => Some(UowStage::Development),
            "awaiting_qa" => Some(UowStage::AwaitingQa),
            "signed_off" => Some(UowStage::SignedOff),
            _ => None,
        }
    }

    /// A monotonic ordinal for the stage (0 = Intake .. 5 = SignedOff). Useful for
    /// "has the story reached at-least stage X" comparisons in the UI without
    /// re-encoding the order. NOT a substitute for the typed transitions — moving
    /// between stages must always go through the transition methods.
    pub fn ordinal(self) -> usize {
        match self {
            UowStage::Intake => 0,
            UowStage::Investigating => 1,
            UowStage::DecisionsApproved => 2,
            UowStage::Development => 3,
            UowStage::AwaitingQa => 4,
            UowStage::SignedOff => 5,
        }
    }

    // ── transitions ────────────────────────────────────────────────────────────

    /// Intake → Investigating. Always allowed from `Intake`; rejected otherwise.
    pub fn begin_investigation(self) -> Result<UowStage, TransitionError> {
        match self {
            UowStage::Intake => Ok(UowStage::Investigating),
            other => Err(TransitionError::WrongStage {
                attempted: "begin_investigation",
                from: other,
                expected: UowStage::Intake,
            }),
        }
    }

    /// Investigating → DecisionsApproved, gated by the decision records.
    ///
    /// Allowed only when the current stage is `Investigating` AND the supplied
    /// decisions pass [`decisions_approved_for_development`] (at least one decision,
    /// every decision `Approved`). This is the no-code-first gate's structural half.
    pub fn approve_decisions(
        self,
        decisions: &[DecisionRecord],
    ) -> Result<UowStage, TransitionError> {
        if self != UowStage::Investigating {
            return Err(TransitionError::WrongStage {
                attempted: "approve_decisions",
                from: self,
                expected: UowStage::Investigating,
            });
        }
        if !decisions_approved_for_development(decisions) {
            return Err(TransitionError::DecisionsNotApproved {
                total: decisions.len(),
                unapproved: decisions.iter().filter(|d| d.needs_review()).count(),
            });
        }
        Ok(UowStage::DecisionsApproved)
    }

    /// DecisionsApproved → Development, with a re-check of the decision gate.
    ///
    /// This is the transition the governed-run start calls. It requires the stage to
    /// be `DecisionsApproved` AND re-verifies the decision gate from the supplied
    /// records, so a stale or hand-edited stage can never let a run start over
    /// unapproved decisions. The double-check is deliberate (defense in depth): the
    /// gate is the product's whole reason to exist, so it is enforced at the point of
    /// no return, not only at the earlier `approve_decisions` step.
    pub fn start_development(
        self,
        decisions: &[DecisionRecord],
    ) -> Result<UowStage, TransitionError> {
        if self != UowStage::DecisionsApproved {
            return Err(TransitionError::WrongStage {
                attempted: "start_development",
                from: self,
                expected: UowStage::DecisionsApproved,
            });
        }
        if !decisions_approved_for_development(decisions) {
            return Err(TransitionError::DecisionsNotApproved {
                total: decisions.len(),
                unapproved: decisions.iter().filter(|d| d.needs_review()).count(),
            });
        }
        Ok(UowStage::Development)
    }

    /// Development → AwaitingQa. Allowed only from `Development`; this is what the run
    /// completion calls once the fleet has walked to its terminal/gating stage.
    pub fn finish_development(self) -> Result<UowStage, TransitionError> {
        match self {
            UowStage::Development => Ok(UowStage::AwaitingQa),
            other => Err(TransitionError::WrongStage {
                attempted: "finish_development",
                from: other,
                expected: UowStage::Development,
            }),
        }
    }

    /// AwaitingQa → SignedOff. The architect's explicit, never-automatic gate.
    /// Allowed only from `AwaitingQa`.
    pub fn sign_off(self) -> Result<UowStage, TransitionError> {
        match self {
            UowStage::AwaitingQa => Ok(UowStage::SignedOff),
            other => Err(TransitionError::WrongStage {
                attempted: "sign_off",
                from: other,
                expected: UowStage::AwaitingQa,
            }),
        }
    }
}

/// Why a lifecycle transition was rejected. Surfaced to the UI so the architect sees
/// exactly what is blocking (e.g. "2 of 3 decisions still need review") rather than a
/// generic failure.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TransitionError {
    /// The transition was attempted from the wrong stage.
    WrongStage {
        /// The transition method name that was attempted.
        attempted: &'static str,
        /// The stage the UoW was actually in.
        from: UowStage,
        /// The stage the transition requires as its starting point.
        expected: UowStage,
    },
    /// The decision gate is not satisfied: either no decisions exist, or at least one
    /// is still `Pending`/`Rejected`. Carries the counts for a precise message.
    DecisionsNotApproved {
        /// Total decision records on the story.
        total: usize,
        /// How many still need the architect's review (Pending or Rejected). When
        /// `total == 0` this is also `0`, but the gate still blocks (no decisions at
        /// all is itself a block).
        unapproved: usize,
    },
}

impl TransitionError {
    /// A human-readable, architect-facing explanation of the block.
    pub fn message(&self) -> String {
        match self {
            TransitionError::WrongStage {
                attempted,
                from,
                expected,
            } => format!(
                "Cannot {attempted}: the story is at \"{}\" but this step requires \"{}\".",
                from.label(),
                expected.label()
            ),
            TransitionError::DecisionsNotApproved { total, unapproved } => {
                if *total == 0 {
                    "Cannot start development: no decisions have been recorded yet. \
                     The investigation must surface at least one decision (even \
                     \"no tradeoffs identified\") and the architect must approve it."
                        .to_string()
                } else {
                    format!(
                        "Cannot start development: {unapproved} of {total} decision(s) \
                         still need the architect's approval. Every decision must be \
                         approved before any code is written."
                    )
                }
            }
        }
    }
}

impl std::fmt::Display for TransitionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for TransitionError {}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_worktracker::investigation::DecisionRecord;
    use chrono::Utc;

    fn pending_decision(slug: &str) -> DecisionRecord {
        DecisionRecord::ai_proposed(
            "CAM-1",
            format!("CAM-1/decision/{slug}"),
            "Some decision",
            "Some question?",
            "Some rationale",
            vec![],
            Utc::now(),
        )
    }

    fn approved_decision(slug: &str) -> DecisionRecord {
        pending_decision(slug).approve(Utc::now())
    }

    // ── wire round-trip ──────────────────────────────────────────────────────

    #[test]
    fn wire_str_and_from_wire_round_trip_all_stages() {
        for stage in [
            UowStage::Intake,
            UowStage::Investigating,
            UowStage::DecisionsApproved,
            UowStage::Development,
            UowStage::AwaitingQa,
            UowStage::SignedOff,
        ] {
            let s = stage.wire_str();
            assert_eq!(UowStage::from_wire(s), Some(stage), "round-trip {s}");
        }
        assert_eq!(UowStage::from_wire("bogus"), None);
    }

    #[test]
    fn serde_matches_wire_str() {
        let json = serde_json::to_string(&UowStage::DecisionsApproved).unwrap();
        assert_eq!(json, "\"decisions_approved\"");
        let back: UowStage = serde_json::from_str(&json).unwrap();
        assert_eq!(back, UowStage::DecisionsApproved);
    }

    #[test]
    fn default_is_intake() {
        assert_eq!(UowStage::default(), UowStage::Intake);
    }

    #[test]
    fn ordinal_is_monotonic() {
        let order = [
            UowStage::Intake,
            UowStage::Investigating,
            UowStage::DecisionsApproved,
            UowStage::Development,
            UowStage::AwaitingQa,
            UowStage::SignedOff,
        ];
        for w in order.windows(2) {
            assert!(w[0].ordinal() < w[1].ordinal(), "{:?} < {:?}", w[0], w[1]);
        }
    }

    // ── begin_investigation ──────────────────────────────────────────────────

    #[test]
    fn begin_investigation_from_intake_ok() {
        assert_eq!(
            UowStage::Intake.begin_investigation(),
            Ok(UowStage::Investigating)
        );
    }

    #[test]
    fn begin_investigation_from_wrong_stage_errors() {
        let err = UowStage::Development.begin_investigation().unwrap_err();
        assert!(matches!(
            err,
            TransitionError::WrongStage {
                attempted: "begin_investigation",
                from: UowStage::Development,
                expected: UowStage::Intake,
            }
        ));
    }

    // ── approve_decisions (the gate) ─────────────────────────────────────────

    #[test]
    fn approve_decisions_blocks_with_no_decisions() {
        let err = UowStage::Investigating.approve_decisions(&[]).unwrap_err();
        assert!(matches!(
            err,
            TransitionError::DecisionsNotApproved {
                total: 0,
                unapproved: 0
            }
        ));
    }

    #[test]
    fn approve_decisions_blocks_with_a_pending_decision() {
        let decisions = vec![approved_decision("a"), pending_decision("b")];
        let err = UowStage::Investigating
            .approve_decisions(&decisions)
            .unwrap_err();
        assert!(matches!(
            err,
            TransitionError::DecisionsNotApproved {
                total: 2,
                unapproved: 1
            }
        ));
    }

    #[test]
    fn approve_decisions_ok_when_all_approved() {
        let decisions = vec![approved_decision("a"), approved_decision("b")];
        assert_eq!(
            UowStage::Investigating.approve_decisions(&decisions),
            Ok(UowStage::DecisionsApproved)
        );
    }

    #[test]
    fn approve_decisions_from_wrong_stage_errors_even_if_approved() {
        let decisions = vec![approved_decision("a")];
        let err = UowStage::Intake
            .approve_decisions(&decisions)
            .unwrap_err();
        assert!(matches!(
            err,
            TransitionError::WrongStage {
                attempted: "approve_decisions",
                ..
            }
        ));
    }

    // ── start_development (the re-checked gate) ──────────────────────────────

    #[test]
    fn start_development_requires_decisions_approved_stage() {
        let decisions = vec![approved_decision("a")];
        let err = UowStage::Investigating
            .start_development(&decisions)
            .unwrap_err();
        assert!(matches!(
            err,
            TransitionError::WrongStage {
                attempted: "start_development",
                ..
            }
        ));
    }

    #[test]
    fn start_development_rechecks_the_decision_gate() {
        // Stage says DecisionsApproved but the records no longer pass (e.g. a decision
        // was re-opened). The re-check must still block.
        let decisions = vec![pending_decision("a")];
        let err = UowStage::DecisionsApproved
            .start_development(&decisions)
            .unwrap_err();
        assert!(matches!(
            err,
            TransitionError::DecisionsNotApproved { .. }
        ));
    }

    #[test]
    fn start_development_ok_from_decisions_approved_with_approved_records() {
        let decisions = vec![approved_decision("a")];
        assert_eq!(
            UowStage::DecisionsApproved.start_development(&decisions),
            Ok(UowStage::Development)
        );
    }

    // ── finish_development ───────────────────────────────────────────────────

    #[test]
    fn finish_development_from_development_ok() {
        assert_eq!(
            UowStage::Development.finish_development(),
            Ok(UowStage::AwaitingQa)
        );
    }

    #[test]
    fn finish_development_from_wrong_stage_errors() {
        let err = UowStage::Intake.finish_development().unwrap_err();
        assert!(matches!(
            err,
            TransitionError::WrongStage {
                attempted: "finish_development",
                ..
            }
        ));
    }

    // ── sign_off ─────────────────────────────────────────────────────────────

    #[test]
    fn sign_off_from_awaiting_qa_ok() {
        assert_eq!(UowStage::AwaitingQa.sign_off(), Ok(UowStage::SignedOff));
    }

    #[test]
    fn sign_off_from_wrong_stage_errors() {
        let err = UowStage::Development.sign_off().unwrap_err();
        assert!(matches!(
            err,
            TransitionError::WrongStage {
                attempted: "sign_off",
                from: UowStage::Development,
                expected: UowStage::AwaitingQa,
            }
        ));
    }

    // ── a full happy-path walk ───────────────────────────────────────────────

    #[test]
    fn full_lifecycle_happy_path() {
        let decisions = vec![approved_decision("a"), approved_decision("b")];

        let s = UowStage::default();
        assert_eq!(s, UowStage::Intake);
        let s = s.begin_investigation().unwrap();
        assert_eq!(s, UowStage::Investigating);
        let s = s.approve_decisions(&decisions).unwrap();
        assert_eq!(s, UowStage::DecisionsApproved);
        let s = s.start_development(&decisions).unwrap();
        assert_eq!(s, UowStage::Development);
        let s = s.finish_development().unwrap();
        assert_eq!(s, UowStage::AwaitingQa);
        let s = s.sign_off().unwrap();
        assert_eq!(s, UowStage::SignedOff);
    }

    // ── error messages ───────────────────────────────────────────────────────

    #[test]
    fn decisions_not_approved_message_distinguishes_empty_from_partial() {
        let empty = TransitionError::DecisionsNotApproved {
            total: 0,
            unapproved: 0,
        };
        assert!(empty.message().contains("no decisions"));

        let partial = TransitionError::DecisionsNotApproved {
            total: 3,
            unapproved: 2,
        };
        let msg = partial.message();
        assert!(msg.contains("2 of 3"));
    }

    #[test]
    fn wrong_stage_message_names_both_stages() {
        let err = TransitionError::WrongStage {
            attempted: "sign_off",
            from: UowStage::Development,
            expected: UowStage::AwaitingQa,
        };
        let msg = err.message();
        assert!(msg.contains("Development"));
        assert!(msg.contains("Awaiting QA"));
    }

    #[test]
    fn transition_error_serializes_with_kind_tag() {
        let err = TransitionError::DecisionsNotApproved {
            total: 1,
            unapproved: 1,
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("\"decisions_not_approved\""));
    }

    // ── exhaustive transition table ──────────────────────────────────────────
    //
    // For EVERY UowStage variant × EVERY transition method: assert the exact
    // outcome. This is the "state machine completeness" test — if a new
    // UowStage variant is ever added, the explicit array below will force a
    // compile error (non-exhaustive match) or obvious test failure, ensuring
    // the table is kept up to date.
    //
    // Transitions that accept `&[DecisionRecord]` are tested with two flavours:
    //   - `approved_decisions` (a non-empty slice where every entry is Approved)
    //   - `bad_decisions`      (a non-empty slice with at least one Pending)
    // so we can exercise both the gate-pass and gate-fail paths from the correct
    // source stage, and confirm that neither flavour helps wrong-stage calls.

    const ALL_STAGES: [UowStage; 6] = [
        UowStage::Intake,
        UowStage::Investigating,
        UowStage::DecisionsApproved,
        UowStage::Development,
        UowStage::AwaitingQa,
        UowStage::SignedOff,
    ];

    // ── begin_investigation: legal only from Intake ──────────────────────────

    #[test]
    fn transition_table_begin_investigation() {
        for stage in ALL_STAGES {
            let result = stage.begin_investigation();
            if stage == UowStage::Intake {
                assert_eq!(
                    result,
                    Ok(UowStage::Investigating),
                    "begin_investigation should succeed from Intake"
                );
            } else {
                let err = result.unwrap_err();
                assert!(
                    matches!(
                        err,
                        TransitionError::WrongStage {
                            attempted: "begin_investigation",
                            expected: UowStage::Intake,
                            ..
                        }
                    ),
                    "begin_investigation from {stage:?} should produce WrongStage(expected=Intake), got {err:?}"
                );
                // Confirm `from` carries the actual stage, not something else.
                if let TransitionError::WrongStage { from, .. } = err {
                    assert_eq!(from, stage);
                }
            }
        }
    }

    // ── approve_decisions: legal only from Investigating + gate satisfied ────

    #[test]
    fn transition_table_approve_decisions_with_approved_records() {
        let good = vec![approved_decision("x"), approved_decision("y")];
        for stage in ALL_STAGES {
            let result = stage.approve_decisions(&good);
            if stage == UowStage::Investigating {
                assert_eq!(
                    result,
                    Ok(UowStage::DecisionsApproved),
                    "approve_decisions with approved records should succeed from Investigating"
                );
            } else {
                let err = result.unwrap_err();
                assert!(
                    matches!(
                        err,
                        TransitionError::WrongStage {
                            attempted: "approve_decisions",
                            expected: UowStage::Investigating,
                            ..
                        }
                    ),
                    "approve_decisions (good records) from {stage:?} should produce WrongStage, got {err:?}"
                );
                if let TransitionError::WrongStage { from, .. } = err {
                    assert_eq!(from, stage);
                }
            }
        }
    }

    #[test]
    fn transition_table_approve_decisions_with_bad_records_from_investigating() {
        // Even the legal source stage must not succeed when the gate is not satisfied.
        let bad = vec![approved_decision("a"), pending_decision("b")];
        let err = UowStage::Investigating
            .approve_decisions(&bad)
            .unwrap_err();
        assert!(
            matches!(
                err,
                TransitionError::DecisionsNotApproved {
                    total: 2,
                    unapproved: 1
                }
            ),
            "gate must block even from Investigating when a decision is still pending: {err:?}"
        );
    }

    #[test]
    fn transition_table_approve_decisions_with_empty_records_from_investigating() {
        // No decisions at all is also a gate failure (gate requires at least one).
        let err = UowStage::Investigating
            .approve_decisions(&[])
            .unwrap_err();
        assert!(
            matches!(
                err,
                TransitionError::DecisionsNotApproved {
                    total: 0,
                    unapproved: 0
                }
            ),
            "gate must block with zero decisions: {err:?}"
        );
    }

    #[test]
    fn transition_table_approve_decisions_wrong_stage_beats_gate() {
        // Wrong-stage check runs before the gate check; bad records from a wrong
        // stage should still give WrongStage, not DecisionsNotApproved.
        let bad = vec![pending_decision("p")];
        for stage in ALL_STAGES {
            if stage == UowStage::Investigating {
                continue; // legal stage, covered above
            }
            let err = stage.approve_decisions(&bad).unwrap_err();
            assert!(
                matches!(err, TransitionError::WrongStage { .. }),
                "approve_decisions with bad records from {stage:?} should produce WrongStage first: {err:?}"
            );
        }
    }

    // ── start_development: legal only from DecisionsApproved + gate satisfied ─

    #[test]
    fn transition_table_start_development_with_approved_records() {
        let good = vec![approved_decision("x")];
        for stage in ALL_STAGES {
            let result = stage.start_development(&good);
            if stage == UowStage::DecisionsApproved {
                assert_eq!(
                    result,
                    Ok(UowStage::Development),
                    "start_development with approved records should succeed from DecisionsApproved"
                );
            } else {
                let err = result.unwrap_err();
                assert!(
                    matches!(
                        err,
                        TransitionError::WrongStage {
                            attempted: "start_development",
                            expected: UowStage::DecisionsApproved,
                            ..
                        }
                    ),
                    "start_development (good records) from {stage:?} should produce WrongStage, got {err:?}"
                );
                if let TransitionError::WrongStage { from, .. } = err {
                    assert_eq!(from, stage);
                }
            }
        }
    }

    #[test]
    fn transition_table_start_development_rechecks_gate() {
        // Stage is correct but records have a pending decision — the re-check must block.
        let bad = vec![pending_decision("p")];
        let err = UowStage::DecisionsApproved
            .start_development(&bad)
            .unwrap_err();
        assert!(
            matches!(err, TransitionError::DecisionsNotApproved { .. }),
            "start_development must recheck the gate even when the stage is correct: {err:?}"
        );
    }

    #[test]
    fn transition_table_start_development_wrong_stage_beats_gate() {
        let bad = vec![pending_decision("p")];
        for stage in ALL_STAGES {
            if stage == UowStage::DecisionsApproved {
                continue;
            }
            let err = stage.start_development(&bad).unwrap_err();
            assert!(
                matches!(err, TransitionError::WrongStage { .. }),
                "start_development with bad records from {stage:?} should produce WrongStage first: {err:?}"
            );
        }
    }

    // ── finish_development: legal only from Development ──────────────────────

    #[test]
    fn transition_table_finish_development() {
        for stage in ALL_STAGES {
            let result = stage.finish_development();
            if stage == UowStage::Development {
                assert_eq!(
                    result,
                    Ok(UowStage::AwaitingQa),
                    "finish_development should succeed from Development"
                );
            } else {
                let err = result.unwrap_err();
                assert!(
                    matches!(
                        err,
                        TransitionError::WrongStage {
                            attempted: "finish_development",
                            expected: UowStage::Development,
                            ..
                        }
                    ),
                    "finish_development from {stage:?} should produce WrongStage(expected=Development), got {err:?}"
                );
                if let TransitionError::WrongStage { from, .. } = err {
                    assert_eq!(from, stage);
                }
            }
        }
    }

    // ── sign_off: legal only from AwaitingQa ────────────────────────────────

    #[test]
    fn transition_table_sign_off() {
        for stage in ALL_STAGES {
            let result = stage.sign_off();
            if stage == UowStage::AwaitingQa {
                assert_eq!(
                    result,
                    Ok(UowStage::SignedOff),
                    "sign_off should succeed from AwaitingQa"
                );
            } else {
                let err = result.unwrap_err();
                assert!(
                    matches!(
                        err,
                        TransitionError::WrongStage {
                            attempted: "sign_off",
                            expected: UowStage::AwaitingQa,
                            ..
                        }
                    ),
                    "sign_off from {stage:?} should produce WrongStage(expected=AwaitingQa), got {err:?}"
                );
                if let TransitionError::WrongStage { from, .. } = err {
                    assert_eq!(from, stage);
                }
            }
        }
    }

    // ── no stage can call multiple transitions successfully in one step ───────
    //
    // Belt-and-suspenders: a correct stage produces exactly ONE Ok target from
    // the transition it is meant for and Err from the other four.

    #[test]
    fn each_stage_has_exactly_one_legal_simple_transition() {
        let good = vec![approved_decision("z")];

        // (stage, which transition gives Ok, expected next stage)
        // Transitions with decision args: approve_decisions / start_development
        // Simple transitions: begin_investigation / finish_development / sign_off

        // Intake: only begin_investigation succeeds.
        assert!(UowStage::Intake.begin_investigation().is_ok());
        assert!(UowStage::Intake.approve_decisions(&good).is_err());
        assert!(UowStage::Intake.start_development(&good).is_err());
        assert!(UowStage::Intake.finish_development().is_err());
        assert!(UowStage::Intake.sign_off().is_err());

        // Investigating: only approve_decisions (with good records) succeeds.
        assert!(UowStage::Investigating.begin_investigation().is_err());
        assert!(UowStage::Investigating.approve_decisions(&good).is_ok());
        assert!(UowStage::Investigating.start_development(&good).is_err());
        assert!(UowStage::Investigating.finish_development().is_err());
        assert!(UowStage::Investigating.sign_off().is_err());

        // DecisionsApproved: only start_development (with good records) succeeds.
        assert!(UowStage::DecisionsApproved.begin_investigation().is_err());
        assert!(UowStage::DecisionsApproved.approve_decisions(&good).is_err());
        assert!(UowStage::DecisionsApproved.start_development(&good).is_ok());
        assert!(UowStage::DecisionsApproved.finish_development().is_err());
        assert!(UowStage::DecisionsApproved.sign_off().is_err());

        // Development: only finish_development succeeds.
        assert!(UowStage::Development.begin_investigation().is_err());
        assert!(UowStage::Development.approve_decisions(&good).is_err());
        assert!(UowStage::Development.start_development(&good).is_err());
        assert!(UowStage::Development.finish_development().is_ok());
        assert!(UowStage::Development.sign_off().is_err());

        // AwaitingQa: only sign_off succeeds.
        assert!(UowStage::AwaitingQa.begin_investigation().is_err());
        assert!(UowStage::AwaitingQa.approve_decisions(&good).is_err());
        assert!(UowStage::AwaitingQa.start_development(&good).is_err());
        assert!(UowStage::AwaitingQa.finish_development().is_err());
        assert!(UowStage::AwaitingQa.sign_off().is_ok());

        // SignedOff: no transition succeeds (terminal state).
        assert!(UowStage::SignedOff.begin_investigation().is_err());
        assert!(UowStage::SignedOff.approve_decisions(&good).is_err());
        assert!(UowStage::SignedOff.start_development(&good).is_err());
        assert!(UowStage::SignedOff.finish_development().is_err());
        assert!(UowStage::SignedOff.sign_off().is_err());
    }
}
