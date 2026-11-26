//! The run figures a saved run's own folder holds (`GET /api/inspections/:id/figures`): the
//! wall-clock time of the sessions that worked on it and where each batch stage's Message Batches
//! stand. Operator-only, never part of the client package. A leaf type so the server that reads
//! the folder and the app's Run figures panel share one definition.
//!
//! Metrics only: no batch id, no repository name, no path. A batch is counted under its stage,
//! never named.

use serde::{Deserialize, Serialize};

/// Where one submitted Message Batch stands, as far as the run recorded it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchState {
    /// The batch was accepted and no poll of it has been recorded since.
    Submitted,
    /// The last recorded poll said the provider was still working on it.
    Processing,
    /// The provider finished it (its results were read, or the last poll said it ended).
    Ended,
}

impl BatchState {
    /// The state a provider's `processing_status` names: `ended` is ended, `in_progress` and
    /// `canceling` are processing. Any other word is not a state this run can vouch for, so it
    /// reads as submitted (accepted, nothing more known).
    pub fn from_provider(processing_status: &str) -> Self {
        match processing_status {
            "ended" => Self::Ended,
            "in_progress" | "canceling" => Self::Processing,
            _ => Self::Submitted,
        }
    }
}

/// How many of one stage's batches stand where.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchStageStatus {
    /// The stage, by the name the saved stage figures carry (`scan_pass`, `calibration`,
    /// `alternatives`, `detail_regeneration`, `group_review`, `fix_generation`, `finder`), or
    /// `other`.
    pub stage: String,
    pub submitted: u32,
    pub processing: u32,
    pub ended: u32,
}

impl BatchStageStatus {
    /// Count one batch in `state`.
    pub fn count(&mut self, state: BatchState) {
        match state {
            BatchState::Submitted => self.submitted += 1,
            BatchState::Processing => self.processing += 1,
            BatchState::Ended => self.ended += 1,
        }
    }
}

/// What a saved run's folder says about its time and its batches.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecordFigures {
    /// The sessions' wall-clock time added up (each session's monotonic elapsed time, so a clock
    /// change does not distort it). `None` when the run saved no session segment (a run saved
    /// before segments existed): not recorded, never zero.
    #[serde(default)]
    pub wall_clock_ms: Option<u64>,
    /// How many sessions worked on the run: the first inspection, then each resume.
    #[serde(default)]
    pub sessions: u32,
    /// The batch stages, in pipeline order. Empty when the run submitted no batch.
    #[serde(default)]
    pub batches: Vec<BatchStageStatus>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provider_words_map_to_the_three_states() {
        assert_eq!(BatchState::from_provider("ended"), BatchState::Ended);
        assert_eq!(
            BatchState::from_provider("in_progress"),
            BatchState::Processing
        );
        assert_eq!(
            BatchState::from_provider("canceling"),
            BatchState::Processing
        );
        assert_eq!(BatchState::from_provider(""), BatchState::Submitted);
    }

    #[test]
    fn a_figures_body_without_wall_clock_reads_as_not_recorded() {
        let back: RunRecordFigures = serde_json::from_str("{}").unwrap();
        assert_eq!(back.wall_clock_ms, None);
        assert!(back.batches.is_empty());
    }
}
