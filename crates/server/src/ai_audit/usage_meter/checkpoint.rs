//! The cost checkpoint on the meter: the finder's piece tally, the projection against the
//! approved amount (the spend limit) and the stop. See [`crate::cost_checkpoint`].
//!
//! The stop is the spend limit's own: the refused part is recorded NOT RUN with the spend-limit
//! class, so the work left is a part a resume retries. A sticky flag makes every later admission
//! refuse, so nothing is paid after the checkpoint stopped the run.

use super::*;
use crate::cost_checkpoint::{
    over_limit, projected_total, CheckpointSink, CheckpointState, CostCheckpoint, ProjectionInput,
    CAUSE_PROJECTED, CHECKPOINT_FILE,
};

impl UsageMeter {
    fn checkpoint_state(&self) -> std::sync::MutexGuard<'_, CheckpointState> {
        self.checkpoint
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Also send every checkpoint to `sink` (the cockpit's job state).
    pub fn set_job_sink(&self, sink: CheckpointSink) {
        *self
            .job_sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = sink;
    }

    /// The finder is about to run `total` pieces, `already_done` of them held by a stored
    /// attempt. Adds to what earlier repos of the run announced.
    pub fn begin_pieces(&self, total: usize, already_done: usize) {
        let mut st = self.checkpoint_state();
        st.pieces_total += total;
        st.pieces_done += already_done.min(total);
    }

    /// `halves` of the pieces just announced are halves of a piece cut in two on a resume. They
    /// are one planned piece's work between them, so the projection counts two as one left.
    pub fn begin_halves(&self, halves: usize) {
        self.checkpoint_state().halves_left += halves;
    }

    /// A half finished (call before [`Self::piece_finished`]).
    pub fn half_finished(&self) {
        let mut st = self.checkpoint_state();
        st.halves_left = st.halves_left.saturating_sub(1);
    }

    /// The repository's pieces are over: no half is waiting any more.
    pub fn end_halves(&self) {
        self.checkpoint_state().halves_left = 0;
    }

    /// One piece finished clean. `cost_usd` is what it cost this attempt (`None` when it was not
    /// paid for here); `problems` is how many problems it found. With a limit set this records
    /// and prints the checkpoint line.
    pub fn piece_finished(&self, cost_usd: Option<f64>, problems: usize) {
        let Some(cap) = self.controls.max_cost_usd else {
            return;
        };
        let cp = {
            let mut st = self.checkpoint_state();
            st.pieces_done += 1;
            st.problems_found += problems;
            if let Some(cost) = cost_usd {
                st.fresh_done += 1;
                st.fresh_cost_usd += cost;
            }
            let mut cp = self.build_checkpoint(&st, cap, None);
            // A piece that was already in flight when the run paused finishes after the pause:
            // the record stays the pause, with the spend and the work done brought up to date.
            if let Some(stop) = &st.stop {
                let before = stop.stopped_before.clone();
                self.mark_stopped(&mut cp, &before);
                st.stop = Some(cp.clone());
            }
            st.latest = Some(cp.clone());
            cp
        };
        self.publish_checkpoint(&cp);
    }

    /// The projection for the state. `later_spent` is `Some` at a stage boundary (what the stages
    /// after the finder have already spent) and `None` while the finder runs.
    fn build_checkpoint(
        &self,
        st: &CheckpointState,
        cap: f64,
        later_spent: Option<f64>,
    ) -> CostCheckpoint {
        let plan = self.controls.checkpoint.plan.clone().unwrap_or_default();
        let spent = self.limit_spend_usd();
        let planned = plan.pieces.max(st.pieces_total);
        // While the finder runs: every piece planned and not done. At a stage boundary the
        // finder is over for the repository: a piece that did not finish is not run again in
        // this attempt, but the pieces of repositories not begun yet still are.
        let pieces_left = if later_spent.is_some() {
            planned.saturating_sub(st.pieces_total)
        } else {
            let left = planned.saturating_sub(st.pieces_done);
            let halves = st.halves_left.min(left);
            left - halves + halves.div_ceil(2)
        };
        let projected = projected_total(&ProjectionInput {
            spent_usd: spent,
            pieces_left,
            finished_pieces: st.fresh_done,
            finished_cost_usd: st.fresh_cost_usd,
            preflight_per_piece_usd: plan.per_piece_usd,
            joints_usd: if later_spent.is_some() {
                0.0
            } else {
                plan.joints_usd
            },
            later_stages_usd: plan.later_stages_usd,
            later_spent_usd: later_spent.unwrap_or(0.0),
            assumed_problems: plan.assumed_problems,
            problems_found: st.problems_found,
            pieces_done: st.pieces_done,
            pieces_planned: planned,
        });
        CostCheckpoint {
            approved_usd: cap,
            spent_usd: spent,
            projected_usd: projected,
            pieces_done: st.pieces_done,
            pieces_left,
            ..CostCheckpoint::default()
        }
    }

    /// Before the finder starts a piece: true when the projected total is over the approved
    /// amount, so the piece must not start. The stop is recorded (once) and sticks. Always false
    /// with no limit set.
    pub fn projection_stops_piece(&self) -> bool {
        let Some(cap) = self.controls.max_cost_usd else {
            return false;
        };
        let cp = {
            let mut st = self.checkpoint_state();
            if st.stop.is_some() {
                return true;
            }
            let mut cp = self.build_checkpoint(&st, cap, None);
            if !over_limit(cp.projected_usd, cap) {
                return false;
            }
            self.mark_stopped(&mut cp, STAGE_FINDER);
            st.stop = Some(cp.clone());
            st.latest = Some(cp.clone());
            cp
        };
        self.record_note(cp.pause_message());
        self.publish_checkpoint(&cp);
        true
    }

    /// At a stage boundary, after the other checks passed: true (and the stage is recorded NOT
    /// RUN) when the projection for the stages still to come is over the approved amount.
    pub(super) fn projection_stops_stage(&self, repo: &str, stage: &str) -> bool {
        let Some(cap) = self.controls.max_cost_usd else {
            return false;
        };
        let spent = self.limit_spend_usd();
        let cp = {
            let mut st = self.checkpoint_state();
            if st.stop.is_some() {
                return true;
            }
            let baseline = *st.later_baseline_usd.get_or_insert(spent);
            let mut cp = self.build_checkpoint(&st, cap, Some((spent - baseline).max(0.0)));
            if !over_limit(cp.projected_usd, cap) {
                return false;
            }
            self.mark_stopped(&mut cp, stage);
            st.stop = Some(cp.clone());
            st.latest = Some(cp.clone());
            cp
        };
        self.record_stage_not_run(repo, stage, false);
        self.record_note(cp.pause_message());
        self.publish_checkpoint(&cp);
        true
    }

    /// Before a stage whose cost is priced from its own input (the group review, from its row
    /// count): true (and the stage is recorded NOT RUN) when the spend plus `stage_usd` is over
    /// the approved amount. The new estimate is that sum.
    pub(crate) fn priced_stage_stops(&self, repo: &str, stage: &str, stage_usd: f64) -> bool {
        let Some(cap) = self.controls.max_cost_usd else {
            return false;
        };
        let cp = {
            let mut st = self.checkpoint_state();
            if st.stop.is_some() {
                return true;
            }
            let mut cp = self.build_checkpoint(&st, cap, Some(0.0));
            cp.projected_usd = cp.spent_usd + stage_usd;
            if !over_limit(cp.projected_usd, cap) {
                return false;
            }
            cp.pieces_left = 0;
            self.mark_stopped(&mut cp, stage);
            st.stop = Some(cp.clone());
            st.latest = Some(cp.clone());
            cp
        };
        self.record_stage_not_run(repo, stage, false);
        self.record_note(cp.pause_message());
        self.publish_checkpoint(&cp);
        true
    }

    /// A stage that runs on a meter continuing the scan's learns the run was paused: the stop is
    /// the scan's checkpoint, carried on its report. Sticky, like the original.
    pub(crate) fn seed_checkpoint_stop(&self, cp: &CostCheckpoint) {
        let mut st = self.checkpoint_state();
        if st.stop.is_none() {
            st.stop = Some(cp.clone());
            st.latest = Some(cp.clone());
        }
    }

    fn mark_stopped(&self, cp: &mut CostCheckpoint, before: &str) {
        cp.stopped = true;
        cp.new_estimate_usd = Some(cp.projected_usd);
        cp.difference_usd = Some(cp.projected_usd - cp.approved_usd);
        cp.stopped_before = before.to_string();
        // The hard limit refuses a new piece once spent plus the pieces in flight x the largest
        // call reaches it: the advised resume limit holds that back too.
        let held = self.held_reserve_micro(&self.budget());
        cp.reserve_usd = held as f64 / 1_000_000.0;
    }

    /// True once the checkpoint stopped the run.
    pub(crate) fn checkpoint_stopped(&self) -> bool {
        self.checkpoint_state().stop.is_some()
    }

    /// The checkpoint that stopped the run, when one did.
    pub fn checkpoint_stop(&self) -> Option<CostCheckpoint> {
        self.checkpoint_state().stop.clone()
    }

    /// The latest checkpoint (progress or stop).
    pub fn latest_checkpoint(&self) -> Option<CostCheckpoint> {
        self.checkpoint_state().latest.clone()
    }

    /// Why a refused part was refused, as every disclosure completes "NOT RUN: <cause> ...": the
    /// projection, the limit, or a cost that could not be determined.
    pub fn stop_cause(&self) -> &'static str {
        if self.cost_unknown() {
            crate::cost_cap::CAUSE_COST_UNKNOWN
        } else if self.checkpoint_stopped() {
            CAUSE_PROJECTED
        } else {
            crate::cost_cap::CAUSE_LIMIT_REACHED
        }
    }

    /// Record the checkpoint in the run folder, print it through the sink and hand it to the
    /// job state. Best effort: a record that cannot be written never stops the run.
    fn publish_checkpoint(&self, cp: &CostCheckpoint) {
        if let Some(folder) = &self.controls.checkpoint.run_folder {
            if let Ok(bytes) = serde_json::to_vec_pretty(cp) {
                let _ = crate::run_store::write_atomic(&folder.join(CHECKPOINT_FILE), &bytes);
            }
        }
        self.controls.checkpoint.sink.send(cp);
        let job = self
            .job_sink
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        job.send(cp);
    }
}
