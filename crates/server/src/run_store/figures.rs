//! The run figures a saved run's folder holds, for the app's Run figures panel
//! (`GET /api/inspections/:id/figures`, GitHub #293): the wall-clock time of the sessions that
//! worked on the run, and where each batch stage's Message Batches stand (#61). The token, call
//! and per-stage time figures are on the report's ledger, which the app already holds; this is
//! the part only the folder has.
//!
//! Operator-only and read-only: nothing here writes a file, and nothing the export reads comes
//! from here. Metrics only: batch ids and the repository part of a checkpoint's name stay inside
//! this module; the answer counts batches per stage and names nothing.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use camerata_api_types::run_figures::{BatchStageStatus, BatchState, RunRecordFigures};

use super::{load_record, RunStoreError};

/// The stages a batch can belong to, in pipeline order (the order the panel lists them), by the
/// names the saved stage figures carry ([`crate::stage_telemetry::Stage`] as serialized).
const BATCH_STAGE_ORDER: [&str; 8] = [
    "scan_pass",
    "calibration",
    "alternatives",
    "detail_regeneration",
    "group_review",
    "fix_generation",
    "finder",
    "other",
];

/// The stage a stage checkpoint's file stem belongs to. A stage on one repo is named
/// `<stage>_<repo>` (`calibration_acme_portal`); the group reviewer's two checkpoints are
/// `group-review` and `group-review-summary`. Only the stage part is kept.
fn stage_of_checkpoint(stem: &str) -> &'static str {
    let head = stem.split_once('_').map_or(stem, |(head, _)| head);
    match head {
        "calibration" => "calibration",
        "alternatives" => "alternatives",
        "detail" => "detail_regeneration",
        "fix" => "fix_generation",
        "finder" => "finder",
        h if h.starts_with("group-review") => "group_review",
        _ => "other",
    }
}

/// True when `id` has the shape of a run id (`20261124T101500Z-a1b2c3`: letters, digits and
/// dashes), so a request can never name a folder outside the runs folder.
pub fn is_run_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The figures of the run saved in `dir`.
///
/// # Errors
/// [`RunStoreError::NotFound`] when `dir` holds no readable `run.json`.
pub fn record_figures(dir: &Path) -> Result<RunRecordFigures, RunStoreError> {
    let record = load_record(dir)?;
    let wall_clock_ms = (!record.segments.is_empty()).then(|| {
        record
            .segments
            .iter()
            .fold(0u64, |sum, s| sum.saturating_add(s.elapsed_ms))
    });
    let sessions = u32::try_from(record.segments.len()).unwrap_or(u32::MAX);
    Ok(RunRecordFigures {
        wall_clock_ms,
        sessions,
        batches: batch_stages(dir),
    })
}

/// Every batch the run submitted, counted under its stage. A stage's checkpoint lists the batches
/// it submitted (and whether each settled); a batch no checkpoint lists is the scan passes' own.
/// Where a batch stands comes from the run's batch file: results read, or the last poll's word.
fn batch_stages(dir: &Path) -> Vec<BatchStageStatus> {
    let stored: BTreeMap<String, BatchState> = super::journal::stored_batch_states(dir)
        .into_iter()
        .collect();
    let mut by_stage: BTreeMap<&'static str, BatchStageStatus> = BTreeMap::new();
    let mut claimed: HashSet<String> = HashSet::new();
    for (stem, batches) in checkpoint_batches(dir) {
        let stage = stage_of_checkpoint(&stem);
        for b in batches {
            if !claimed.insert(b.batch_id.clone()) {
                continue;
            }
            let state = if b.settled {
                BatchState::Ended
            } else {
                stored
                    .get(&b.batch_id)
                    .copied()
                    .unwrap_or(BatchState::Submitted)
            };
            by_stage.entry(stage).or_default().count(state);
        }
    }
    for (id, state) in &stored {
        if !claimed.contains(id) {
            by_stage.entry("scan_pass").or_default().count(*state);
        }
    }
    BATCH_STAGE_ORDER
        .iter()
        .filter_map(|stage| {
            by_stage.remove(stage).map(|mut s| {
                s.stage = (*stage).to_string();
                s
            })
        })
        .collect()
}

/// The batches each stage checkpoint of the run lists, by file stem, in name order.
fn checkpoint_batches(dir: &Path) -> Vec<(String, Vec<crate::checkpoint::BatchRecord>)> {
    let folder = crate::checkpoint::checkpoint_dir_of(dir);
    let Ok(entries) = std::fs::read_dir(&folder) else {
        return Vec::new();
    };
    let mut stems: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_suffix(".batches.json"))
                .map(str::to_string)
        })
        .collect();
    stems.sort();
    stems
        .into_iter()
        .map(|stem| {
            let batches = crate::checkpoint::StageCheckpoint::new(&folder, &stem).batches();
            (stem, batches)
        })
        .collect()
}

#[cfg(test)]
#[path = "figures_tests.rs"]
mod tests;
