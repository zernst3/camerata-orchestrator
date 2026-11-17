//! The cost checkpoint: a live projection of what the run will cost against the amount the
//! operator approved (the spend limit), and the pause that stops a run BEFORE it goes past it.
//!
//! There is no second stopping mechanism. The approved amount is the run's spend limit; a
//! projection over it stops the run through the same spend-limit stop (the part is recorded as
//! NOT RUN with the spend-limit class, so the work left is a part a resume retries). Only the
//! reason differs ([`CAUSE_PROJECTED`]). A run with no spend limit has no checkpoint.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// The cause the disclosures name when the projection, not the spend, stopped the run. No dollar
/// figure: the text reaches the client report.
pub const CAUSE_PROJECTED: &str = "the run was paused because its projected cost went over the \
                                  approved amount";

/// The file in the run folder that holds the latest checkpoint.
pub const CHECKPOINT_FILE: &str = "cost_checkpoint.json";

/// The pre-flight figures the projection falls back on. All dollars.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CheckpointPlan {
    /// The estimate's cost of one finder piece, used while no piece has finished in this run.
    pub per_piece_usd: f64,
    /// The estimate's cost of the stages after the finder.
    pub later_stages_usd: f64,
    /// The number of problems the estimate assumed, when it assumed one. The later stages are
    /// scaled by the problems found so far against it; without it they are not scaled.
    pub assumed_problems: Option<usize>,
}

type SinkFn = dyn Fn(&CostCheckpoint) + Send + Sync;

/// Receives every checkpoint as it is made (the command line prints it).
#[derive(Clone, Default)]
pub struct CheckpointSink(Option<Arc<SinkFn>>);

impl CheckpointSink {
    pub fn new(f: impl Fn(&CostCheckpoint) + Send + Sync + 'static) -> Self {
        Self(Some(Arc::new(f)))
    }

    pub(crate) fn send(&self, cp: &CostCheckpoint) {
        if let Some(f) = &self.0 {
            f(cp);
        }
    }
}

impl std::fmt::Debug for CheckpointSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "CheckpointSink(set)"
        } else {
            "CheckpointSink(none)"
        })
    }
}

impl PartialEq for CheckpointSink {
    fn eq(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

/// Everything about the checkpoint that travels on [`crate::cost_cap::RunControls`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CheckpointSettings {
    pub plan: Option<CheckpointPlan>,
    /// The run's folder: the latest checkpoint is written there ([`CHECKPOINT_FILE`]).
    pub run_folder: Option<std::path::PathBuf>,
    pub sink: CheckpointSink,
}

/// The inputs of [`projected_total`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProjectionInput {
    /// What the run has spent, counting a resume's stored spend.
    pub spent_usd: f64,
    /// Pieces not yet finished (in flight ones included).
    pub pieces_left: usize,
    /// Pieces this run finished by paying for them, and what they cost.
    pub finished_pieces: usize,
    pub finished_cost_usd: f64,
    pub preflight_per_piece_usd: f64,
    pub later_stages_usd: f64,
    pub assumed_problems: Option<usize>,
    pub problems_found: usize,
}

/// Projected total = spent so far + (pieces left x the average cost of the pieces finished in
/// this run, or the pre-flight per-piece figure while none has finished) + the pre-flight figure
/// for the stages after the finder (scaled by problems found against the problems the estimate
/// assumed, when it assumed any).
#[must_use]
pub fn projected_total(i: &ProjectionInput) -> f64 {
    let per_piece = if i.finished_pieces > 0 {
        i.finished_cost_usd / i.finished_pieces as f64
    } else {
        i.preflight_per_piece_usd
    };
    let later = match i.assumed_problems {
        Some(assumed) if assumed > 0 => {
            i.later_stages_usd * i.problems_found as f64 / assumed as f64
        }
        _ => i.later_stages_usd,
    };
    i.spent_usd + i.pieces_left as f64 * per_piece + later
}

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
            "Spent ${:.2} of ${:.2} approved; projected ${:.2}",
            self.spent_usd, self.approved_usd, self.projected_usd
        )
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
            ceil_cents(estimate),
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

/// The meter's running tally of the finder's pieces.
#[derive(Debug, Default)]
pub(crate) struct CheckpointState {
    pub pieces_total: usize,
    pub pieces_done: usize,
    pub fresh_done: usize,
    pub fresh_cost_usd: f64,
    pub problems_found: usize,
    /// Spend when the first stage after the finder asked, so a later stage is checked against
    /// what the stages still have to spend.
    pub later_baseline_usd: Option<f64>,
    /// The checkpoint that stopped the run, once one did. Sticky.
    pub stop: Option<CostCheckpoint>,
    pub latest: Option<CostCheckpoint>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> ProjectionInput {
        ProjectionInput {
            spent_usd: 10.0,
            pieces_left: 4,
            finished_pieces: 2,
            finished_cost_usd: 6.0,
            preflight_per_piece_usd: 1.0,
            later_stages_usd: 0.0,
            assumed_problems: None,
            problems_found: 0,
        }
    }

    #[test]
    fn the_average_of_the_finished_pieces_prices_the_pieces_left() {
        // 10 spent + 4 left x (6 / 2) = 22.
        assert!((projected_total(&base()) - 22.0).abs() < 1e-9);
    }

    #[test]
    fn the_pre_flight_figure_prices_the_pieces_left_while_none_has_finished() {
        let i = ProjectionInput {
            finished_pieces: 0,
            finished_cost_usd: 0.0,
            spent_usd: 0.0,
            pieces_left: 5,
            preflight_per_piece_usd: 2.0,
            ..base()
        };
        assert!((projected_total(&i) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn the_later_stages_are_added_unscaled_without_an_assumption() {
        let i = ProjectionInput {
            later_stages_usd: 7.0,
            problems_found: 50,
            ..base()
        };
        assert!((projected_total(&i) - 29.0).abs() < 1e-9);
    }

    #[test]
    fn the_later_stages_scale_by_problems_found_against_the_assumption() {
        let i = ProjectionInput {
            later_stages_usd: 8.0,
            assumed_problems: Some(10),
            problems_found: 15,
            ..base()
        };
        // 22 + 8 x 15 / 10 = 34.
        assert!((projected_total(&i) - 34.0).abs() < 1e-9);
        let none_yet = ProjectionInput {
            problems_found: 0,
            ..i
        };
        assert!((projected_total(&none_yet) - 22.0).abs() < 1e-9);
    }

    #[test]
    fn nothing_left_projects_the_spend() {
        let i = ProjectionInput {
            pieces_left: 0,
            ..base()
        };
        assert!((projected_total(&i) - 10.0).abs() < 1e-9);
    }

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
            "Spent $231.40 of $260.00 approved; projected $318.20"
        );
    }
}
