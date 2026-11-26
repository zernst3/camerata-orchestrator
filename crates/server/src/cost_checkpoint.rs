//! The cost checkpoint: a live projection of what the run will cost against the amount the
//! operator approved (the spend limit), and the pause that stops a run BEFORE it goes past it.
//!
//! There is no second stopping mechanism. The approved amount is the run's spend limit; a
//! projection over it stops the run through the same spend-limit stop (the part is recorded as
//! NOT RUN with the spend-limit class, so the work left is a part a resume retries). Only the
//! reason differs ([`CAUSE_PROJECTED`]). A run with no spend limit has no checkpoint.

use std::sync::Arc;

/// The cause the disclosures name when the projection, not the spend, stopped the run. No dollar
/// figure: the text reaches the client report.
pub const CAUSE_PROJECTED: &str = "the run was paused because its projected cost went over the \
                                  approved amount";

/// The file in the run folder that holds the latest checkpoint.
pub const CHECKPOINT_FILE: &str = "cost_checkpoint.json";

/// The pre-flight figures the projection falls back on. All dollars.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CheckpointPlan {
    /// The pieces the whole run plans (every repository). The projection never counts fewer than
    /// this, so the repositories not begun yet are priced.
    pub pieces: usize,
    /// The estimate's cost of one finder piece, used while no piece has finished in this run.
    pub per_piece_usd: f64,
    /// The estimate's cost of the joints calls after the pieces. Counted while the finder runs,
    /// never at a stage boundary (the joints are over by then).
    pub joints_usd: f64,
    /// The estimate's cost of the stages after the finder.
    pub later_stages_usd: f64,
    /// The number of problems the estimate assumed, when it assumed one. The later stages are
    /// scaled by the problems found so far against it; without it they are not scaled.
    pub assumed_problems: Option<usize>,
}

type SinkFn = dyn Fn(&CostCheckpoint) + Send + Sync;

impl CheckpointPlan {
    /// The plan a resume starts from: the checkpoint the earlier attempt left in the run folder
    /// (pieces = done + left, per piece = spent / done). `None` when it holds nothing to go on.
    #[must_use]
    pub fn from_stored(folder: &std::path::Path) -> Option<Self> {
        let bytes = std::fs::read(folder.join(CHECKPOINT_FILE)).ok()?;
        let cp: CostCheckpoint = serde_json::from_slice(&bytes).ok()?;
        let pieces = cp.pieces_done + cp.pieces_left;
        if cp.pieces_done == 0 || pieces == 0 {
            return None;
        }
        Some(Self {
            pieces,
            per_piece_usd: cp.spent_usd / cp.pieces_done as f64,
            ..Self::default()
        })
    }
}

/// True when `projected` is over `limit`, compared at whole cents, so a limit typed from an
/// estimate shown to the cent never pauses with "(+$0.00)".
#[must_use]
pub fn over_limit(projected: f64, limit: f64) -> bool {
    (projected * 100.0).round() > (limit * 100.0).round()
}

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
    /// Pieces not yet finished (in flight ones included); a half-done split piece is a fraction.
    pub pieces_left: f64,
    /// Pieces this run finished by paying for them (a half counts half), and what they cost.
    pub finished_pieces: f64,
    pub finished_cost_usd: f64,
    pub preflight_per_piece_usd: f64,
    /// The joints calls still to come (zero at a stage boundary).
    pub joints_usd: f64,
    pub later_stages_usd: f64,
    /// What the stages after the finder have spent already.
    pub later_spent_usd: f64,
    pub assumed_problems: Option<usize>,
    pub problems_found: usize,
    /// Pieces done (held ones included) and planned: the problems found so far are scaled to the
    /// planned pieces (found x planned / done) before they are set against the assumption.
    pub pieces_done: usize,
    pub pieces_planned: usize,
}

/// Finished pieces from which the measured average prices the pieces left on its own.
pub const SETTLED_AFTER_PIECES: usize = 5;

/// Projected total = spent so far + (pieces left x the average cost of the pieces finished in
/// this run, blended with the pre-flight per-piece figure until [`SETTLED_AFTER_PIECES`] have
/// finished, and the pre-flight figure alone while none has) + the pre-flight figure
/// for the stages after the finder (scaled by problems found against the problems the estimate
/// assumed, when it assumed any).
#[must_use]
pub fn projected_total(i: &ProjectionInput) -> f64 {
    let per_piece = if i.finished_pieces > 0.0 {
        let measured = i.finished_cost_usd / i.finished_pieces;
        if i.preflight_per_piece_usd > 0.0 {
            // A small sample is a poor guide: the measured average takes over from the pre-flight
            // figure gradually (its weight is the square of the share of the settling count
            // finished), and alone from SETTLED_AFTER_PIECES finished pieces. A finished half
            // counts as half a piece.
            let share = (i.finished_pieces / SETTLED_AFTER_PIECES as f64).min(1.0);
            let weight = share * share;
            weight * measured + (1.0 - weight) * i.preflight_per_piece_usd
        } else {
            measured
        }
    } else {
        i.preflight_per_piece_usd
    };
    let problems = if i.pieces_done > 0 && i.pieces_planned > i.pieces_done {
        i.problems_found as f64 * i.pieces_planned as f64 / i.pieces_done as f64
    } else {
        i.problems_found as f64
    };
    let later = match i.assumed_problems {
        Some(assumed) if assumed > 0 => i.later_stages_usd * problems / assumed as f64,
        _ => i.later_stages_usd,
    };
    let later = (later - i.later_spent_usd).max(0.0);
    i.spent_usd + i.pieces_left * per_piece + i.joints_usd + later
}

pub use camerata_api_types::cost_checkpoint::CostCheckpoint;

/// The meter's running tally of the finder's pieces.
#[derive(Debug, Default)]
pub(crate) struct CheckpointState {
    pub pieces_total: usize,
    pub pieces_done: usize,
    /// Works of this attempt that are halves of a piece an earlier attempt could not finish, and
    /// how many of them are done (held ones included). A planned piece is counted once however
    /// many halves it was cut in, so a half counts as half a piece done and half a piece planned.
    pub halves_total: usize,
    pub halves_done: usize,
    /// The halves this attempt finished by paying for them.
    pub fresh_halves: usize,
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
            pieces_left: 4.0,
            finished_pieces: SETTLED_AFTER_PIECES as f64,
            finished_cost_usd: 15.0,
            preflight_per_piece_usd: 1.0,
            joints_usd: 0.0,
            later_stages_usd: 0.0,
            later_spent_usd: 0.0,
            assumed_problems: None,
            problems_found: 0,
            pieces_done: 0,
            pieces_planned: 0,
        }
    }

    #[test]
    fn the_average_of_the_finished_pieces_prices_the_pieces_left() {
        // 10 spent + 4 left x (15 / 5) = 22.
        assert!((projected_total(&base()) - 22.0).abs() < 1e-9);
    }

    /// Ten pieces estimated at 1 each, a limit of 1.5 times that total. The first piece to finish
    /// cost three times the estimate: a sample of one must not pause the run.
    #[test]
    fn one_dear_piece_among_ten_does_not_pause_a_run_with_headroom() {
        let one_dear = ProjectionInput {
            spent_usd: 3.0,
            pieces_left: 9.0,
            finished_pieces: 1.0,
            finished_cost_usd: 3.0,
            preflight_per_piece_usd: 1.0,
            ..base()
        };
        assert!(!over_limit(projected_total(&one_dear), 15.0));
        // The same pieces, all five finished dear: the measurement now decides alone.
        let five_dear = ProjectionInput {
            spent_usd: 15.0,
            pieces_left: 5.0,
            finished_pieces: 5.0,
            finished_cost_usd: 15.0,
            ..one_dear
        };
        assert!((projected_total(&five_dear) - 30.0).abs() < 1e-9);
        assert!(over_limit(projected_total(&five_dear), 15.0));
    }

    /// The ramp between one piece and five: the measured average (3 a piece) against the estimate
    /// (1 a piece) weighs (finished / 5) squared, so 16 percent at two pieces and 64 at four.
    #[test]
    fn the_measured_average_gains_weight_between_one_and_five_pieces() {
        let at = |finished: usize| ProjectionInput {
            spent_usd: 0.0,
            pieces_left: 10.0,
            finished_pieces: finished as f64,
            finished_cost_usd: 3.0 * finished as f64,
            preflight_per_piece_usd: 1.0,
            ..base()
        };
        // 10 x (0.16 x 3 + 0.84 x 1) = 13.2, and 10 x (0.64 x 3 + 0.36 x 1) = 22.8.
        assert!((projected_total(&at(2)) - 13.2).abs() < 1e-9);
        assert!((projected_total(&at(4)) - 22.8).abs() < 1e-9);
        assert!((projected_total(&at(5)) - 30.0).abs() < 1e-9);
    }

    #[test]
    fn the_pre_flight_figure_prices_the_pieces_left_while_none_has_finished() {
        let i = ProjectionInput {
            finished_pieces: 0.0,
            finished_cost_usd: 0.0,
            spent_usd: 0.0,
            pieces_left: 5.0,
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
            pieces_left: 0.0,
            ..base()
        };
        assert!((projected_total(&i) - 10.0).abs() < 1e-9);
    }

    #[test]
    fn problems_found_so_far_scale_to_the_planned_pieces() {
        let i = ProjectionInput {
            later_stages_usd: 8.0,
            assumed_problems: Some(10),
            problems_found: 5,
            pieces_done: 2,
            pieces_planned: 4,
            pieces_left: 0.0,
            spent_usd: 0.0,
            ..base()
        };
        // 5 found in 2 of 4 pieces: 10 expected, so all $8.
        assert!((projected_total(&i) - 8.0).abs() < 1e-9);
    }

    #[test]
    fn the_joints_figure_counts_and_the_later_spend_comes_off() {
        let i = ProjectionInput {
            pieces_left: 0.0,
            joints_usd: 1.5,
            later_stages_usd: 4.0,
            later_spent_usd: 1.0,
            ..base()
        };
        assert!((projected_total(&i) - 14.5).abs() < 1e-9);
    }

    #[test]
    fn the_limit_is_compared_at_whole_cents() {
        assert!(!over_limit(318.204, 318.20));
        assert!(over_limit(318.21, 318.20));
        assert!(!over_limit(5.0, 5.0));
    }

    #[test]
    fn a_stored_checkpoint_seeds_the_plan() {
        let dir = tempfile::tempdir().unwrap();
        assert!(CheckpointPlan::from_stored(dir.path()).is_none());
        let cp = CostCheckpoint {
            spent_usd: 3.0,
            pieces_done: 1,
            pieces_left: 3,
            ..CostCheckpoint::default()
        };
        std::fs::write(
            dir.path().join(CHECKPOINT_FILE),
            serde_json::to_vec(&cp).unwrap(),
        )
        .unwrap();
        let plan = CheckpointPlan::from_stored(dir.path()).unwrap();
        assert_eq!(plan.pieces, 4);
        assert!((plan.per_piece_usd - 3.0).abs() < 1e-9);
    }

    fn store(dir: &std::path::Path, spent_usd: f64, pieces_done: usize, pieces_left: usize) {
        let cp = CostCheckpoint {
            spent_usd,
            pieces_done,
            pieces_left,
            ..CostCheckpoint::default()
        };
        std::fs::write(dir.join(CHECKPOINT_FILE), serde_json::to_vec(&cp).unwrap()).unwrap();
    }

    /// A checkpoint with nothing finished has no per-piece figure to resume from (spent / 0), so
    /// it gives no plan, whether or not pieces are left.
    #[test]
    fn a_stored_checkpoint_with_no_piece_done_gives_no_plan() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), 3.0, 0, 3);
        assert!(CheckpointPlan::from_stored(dir.path()).is_none());
        let none_left = tempfile::tempdir().unwrap();
        store(none_left.path(), 0.0, 0, 0);
        assert!(CheckpointPlan::from_stored(none_left.path()).is_none());
    }

    /// With more than one piece done, spent divided by done differs from spent times done, so a
    /// wrong operator (multiply for divide) shows. Pieces planned are done plus left.
    #[test]
    fn a_stored_checkpoint_with_several_pieces_done_seeds_spent_over_done() {
        let dir = tempfile::tempdir().unwrap();
        store(dir.path(), 6.0, 2, 3);
        let plan = CheckpointPlan::from_stored(dir.path()).unwrap();
        assert_eq!(plan.pieces, 5);
        // 6 / 2 = 3 (6 x 2 would be 12).
        assert!((plan.per_piece_usd - 3.0).abs() < 1e-9);
        let three = tempfile::tempdir().unwrap();
        store(three.path(), 3.0, 3, 1);
        let plan = CheckpointPlan::from_stored(three.path()).unwrap();
        assert_eq!(plan.pieces, 4);
        // 3 / 3 = 1 (3 x 3 would be 9).
        assert!((plan.per_piece_usd - 1.0).abs() < 1e-9);
    }

    /// An estimate that assumed zero problems has nothing to scale against: the later stages are
    /// added unscaled and the projection stays finite (no division by zero).
    #[test]
    fn an_assumption_of_zero_problems_adds_the_later_stages_unscaled() {
        let i = ProjectionInput {
            later_stages_usd: 8.0,
            assumed_problems: Some(0),
            problems_found: 5,
            ..base()
        };
        let total = projected_total(&i);
        assert!(total.is_finite());
        // 22 + 8 = 30.
        assert!((total - 30.0).abs() < 1e-9);
    }

    /// The same with no problem found: 0 / 0 must not turn the later stages into nothing.
    #[test]
    fn an_assumption_of_zero_problems_with_none_found_still_counts_the_later_stages() {
        let i = ProjectionInput {
            later_stages_usd: 8.0,
            assumed_problems: Some(0),
            problems_found: 0,
            ..base()
        };
        assert!((projected_total(&i) - 30.0).abs() < 1e-9);
    }
}
