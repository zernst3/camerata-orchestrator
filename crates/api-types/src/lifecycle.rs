//! `UowStage` — the pure, serde-only lifecycle stage enum, relocated here (Phase A of
//! the DTO extraction) from `camerata_app_core::lifecycle`, which re-exports it so
//! `camerata_app_core::lifecycle::UowStage` call sites resolve unchanged.
//!
//! Only the enum plus its PURE inherent impl (`label`, `wire_str`, `from_wire`,
//! `ordinal`) live here. The stage-transition methods (`begin_investigation`,
//! `approve_decisions`, `start_development`, `finish_development`, `sign_off`) and
//! `TransitionError` STAY in `camerata_app_core::lifecycle` — they import
//! `camerata_worktracker::investigation::*`, a domain dependency this pure serde leaf
//! crate must not carry. `camerata_app_core::lifecycle` carries them forward as a local
//! `UowTransitions` extension trait implemented for this type (legal under Rust's
//! orphan rules: the trait is local even though the type is foreign), so every existing
//! `stage.begin_investigation()`-style call site keeps compiling unchanged.

use serde::{Deserialize, Serialize};

/// The dev-side lifecycle stage of a Unit of Work.
///
/// This is the SECOND status carried on a UoW, orthogonal to (but richer than) the
/// coarse `crate::uow::DevStatus` (New / InProgress / Done). `DevStatus` is the
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
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
