//! The cost checkpoint a run publishes (`cost_checkpoint.json`, `JobState.cost_checkpoint`): where
//! the run stands against the amount the operator approved, and the pause that stops it before it
//! goes past it. A leaf type so the server that makes it and the UI that shows it share one
//! definition, and the pause wording and the advised resume limit have one source.

use serde::{Deserialize, Serialize};

/// One checkpoint: where the run stands against the amount approved.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct CostCheckpoint {
    pub approved_usd: f64,
    pub spent_usd: f64,
    pub projected_usd: f64,
    pub pieces_done: usize,
    pub pieces_left: usize,
    /// True when this checkpoint stopped the run.
    pub stopped: bool,
    /// On a stop: the new estimate and how far it is over the approved amount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_estimate_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difference_usd: Option<f64>,
    /// The stage the run stopped before (`agent finder` or a later stage's name).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stopped_before: String,
    /// What the hard limit holds back for the pieces in flight (the admission rule refuses a new
    /// piece once spent plus pieces in flight x the largest call reaches the limit). The advised
    /// resume limit adds it, so a resume at exactly that figure is not refused its last pieces.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reserve_usd: f64,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// A dollar figure rounded UP to whole cents, so a limit set to it covers the estimate.
fn ceil_cents(usd: f64) -> f64 {
    (usd * 100.0 - 1e-6).ceil() / 100.0
}

impl CostCheckpoint {
    /// The line printed after each finished piece.
    #[must_use]
    pub fn progress_line(&self) -> String {
        format!(
            "Spent ${:.2} of ${:.2} approved; projected ${:.2} for the finder stage",
            self.spent_usd, self.approved_usd, self.projected_usd
        )
    }

    /// The spend limit to resume with: the new estimate plus the reserve, rounded up to the
    /// cent. The figure [`Self::pause_message`] names.
    #[must_use]
    pub fn resume_limit_usd(&self) -> f64 {
        ceil_cents(self.new_estimate_usd.unwrap_or(self.projected_usd) + self.reserve_usd)
    }

    /// The pause message, in operator words that do not name a client.
    #[must_use]
    pub fn pause_message(&self) -> String {
        let left = if self.pieces_left == 0 {
            "the stages after the finder left".to_string()
        } else if self.pieces_left == 1 {
            "1 piece left".to_string()
        } else {
            format!("{} pieces left", self.pieces_left)
        };
        let estimate = self.new_estimate_usd.unwrap_or(self.projected_usd);
        let diff = self
            .difference_usd
            .unwrap_or(self.projected_usd - self.approved_usd);
        format!(
            "Paused before going over the approved amount. Approved ${:.2}, spent ${:.2}, {left}. \
             New estimate ${estimate:.2} (+${diff:.2}). Approve to continue: resume with a spend \
             limit of at least ${:.2}.",
            self.approved_usd,
            self.spent_usd,
            self.resume_limit_usd(),
        )
    }

    /// What the command line prints for this checkpoint.
    #[must_use]
    pub fn operator_text(&self) -> String {
        if self.stopped {
            self.pause_message()
        } else {
            self.progress_line()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stopped() -> CostCheckpoint {
        CostCheckpoint {
            approved_usd: 260.0,
            spent_usd: 231.40,
            projected_usd: 318.20,
            pieces_done: 6,
            pieces_left: 4,
            stopped: true,
            new_estimate_usd: Some(318.20),
            difference_usd: Some(58.20),
            stopped_before: "agent finder".into(),
            reserve_usd: 0.0,
        }
    }

    #[test]
    fn the_pause_message_is_the_operator_text() {
        assert_eq!(
            stopped().pause_message(),
            "Paused before going over the approved amount. Approved $260.00, spent $231.40, 4 pieces left. New estimate $318.20 (+$58.20). Approve to continue: resume with a spend limit of at least $318.20."
        );
    }

    #[test]
    fn the_pause_message_has_no_dashes_and_no_serial_comma() {
        for pieces_left in [0, 1, 4] {
            let text = CostCheckpoint {
                pieces_left,
                ..stopped()
            }
            .pause_message();
            assert!(
                !text.contains('\u{2014}') && !text.contains('\u{2013}'),
                "{text}"
            );
            assert!(!text.contains(", and"), "{text}");
            assert!(!text.contains(" - "), "{text}");
        }
        assert!(CostCheckpoint {
            pieces_left: 1,
            ..stopped()
        }
        .pause_message()
        .contains("1 piece left"));
        assert!(CostCheckpoint {
            pieces_left: 0,
            ..stopped()
        }
        .pause_message()
        .contains("stages after the finder left"));
    }

    #[test]
    fn the_resume_limit_rounds_up_to_the_cent() {
        let cp = CostCheckpoint {
            new_estimate_usd: Some(318.201),
            projected_usd: 318.201,
            difference_usd: Some(58.201),
            ..stopped()
        };
        assert!(
            cp.pause_message().contains("at least $318.21."),
            "{}",
            cp.pause_message()
        );
    }

    #[test]
    fn the_progress_line_names_spent_approved_and_projected() {
        let cp = CostCheckpoint {
            stopped: false,
            ..stopped()
        };
        assert_eq!(
            cp.operator_text(),
            "Spent $231.40 of $260.00 approved; projected $318.20 for the finder stage"
        );
    }

    #[test]
    fn the_advised_resume_limit_includes_the_hard_limits_reserve() {
        let cp = CostCheckpoint {
            reserve_usd: 2.5,
            ..stopped()
        };
        let text = cp.pause_message();
        assert!(text.contains("New estimate $318.20 (+$58.20)"), "{text}");
        assert!(text.contains("at least $320.70."), "{text}");
    }

    #[test]
    fn the_progress_line_says_it_covers_the_finder_stage() {
        let cp = CostCheckpoint {
            stopped: false,
            ..stopped()
        };
        assert!(cp.progress_line().ends_with("for the finder stage"));
    }

    #[test]
    fn the_resume_limit_is_the_figure_the_pause_message_names() {
        let cp = CostCheckpoint {
            reserve_usd: 2.5,
            ..stopped()
        };
        assert!((cp.resume_limit_usd() - 320.70).abs() < 1e-9);
        assert!(cp.pause_message().contains("at least $320.70."));
    }
}
