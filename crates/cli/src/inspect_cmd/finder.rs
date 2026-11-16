//! `camerata inspect --finder agent|engine` and its companions: the settings, the checks that
//! refuse a bad combination before any spend, the agent a run uses and the comparison of two runs.
//!
//! The agent finder itself lives in `camerata_server::ai_audit::agent_finder`. This module is the
//! command line glue only: it carries the flags, builds the run's piece agent from the saved key
//! and the run's backend, records a run's problems beside its ledger, and reads an earlier run's
//! problems back for `--compare-with`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use camerata_server::ai_audit::agent_finder::run_diff::{self, RunDiff};
use camerata_server::ai_audit::agent_finder::{FinderChoice, PieceAgent};
use camerata_server::onboard::Finding;

/// The finder flags of `camerata inspect`. Saved with the run, so a resume uses the same finder.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FinderArgs {
    /// `--finder agent|engine` (default `engine`, the scan passes).
    #[serde(default)]
    pub finder: FinderChoice,
    /// `--finder-model`: the agent finder's model. `None`/blank = the built-in default. Never the
    /// environment.
    #[serde(default)]
    pub model: Option<String>,
    /// `--finder-force-pieces K` (internal runs only): split every unit into at least K pieces.
    #[serde(default)]
    pub force_pieces: Option<usize>,
    /// `--compare-with <state folder>`: after the run, compare its problems with the ones an
    /// earlier agent finder run left in that state folder and write the difference as an
    /// operator note beside the run's ledger (never into the export).
    #[serde(default)]
    pub compare_with: Option<PathBuf>,
}

/// Parse `--finder`.
pub fn parse_finder(s: &str) -> Result<FinderChoice, String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "agent" => Ok(FinderChoice::Agent),
        "engine" => Ok(FinderChoice::Engine),
        other => Err(format!(
            "unknown --finder `{other}` (expected `agent` or `engine`)"
        )),
    }
}

/// Where a run's problems are recorded for a later `--compare-with`: beside its ledger.
pub fn finder_run_path(ledger_file: &Path) -> PathBuf {
    let stem = ledger_file
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".ledger.json"))
        .unwrap_or("run");
    ledger_file.with_file_name(format!("{stem}.finder-run.json"))
}

/// Record the problems of a finished agent finder run (operator side; never exported).
pub fn write_finder_run(path: &Path, findings: &[Finding]) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(findings).map_err(|e| e.to_string())?;
    std::fs::write(path, bytes).map_err(|e| format!("could not write {}: {e}", path.display()))
}

/// Read the problems an earlier run left in `folder`: every `*.finder-run.json`, or when there is
/// none the findings of every `*.cache.json` (the other finder's scan cache). Empty folders and
/// folders that do not exist are an error: a comparison with nothing is never silent.
pub fn read_state_folder(folder: &Path) -> Result<Vec<Finding>, String> {
    let entries = std::fs::read_dir(folder)
        .map_err(|e| format!("--compare-with {} cannot be read: {e}", folder.display()))?;
    let mut runs: Vec<PathBuf> = Vec::new();
    let mut caches: Vec<PathBuf> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        if name.ends_with(".finder-run.json") {
            runs.push(path);
        } else if name.ends_with(".cache.json") {
            caches.push(path);
        }
    }
    runs.sort();
    caches.sort();
    let mut out: Vec<Finding> = Vec::new();
    if !runs.is_empty() {
        for p in &runs {
            let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
            let mut rows: Vec<Finding> = serde_json::from_slice(&bytes)
                .map_err(|e| format!("{} is not a run record: {e}", p.display()))?;
            out.append(&mut rows);
        }
        return Ok(out);
    }
    for p in &caches {
        let bytes = std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let manifest: camerata_server::scan_cache::ScanManifest = serde_json::from_slice(&bytes)
            .map_err(|e| format!("{} is not a scan cache: {e}", p.display()))?;
        out.extend(manifest.findings);
    }
    if caches.is_empty() {
        return Err(format!(
            "--compare-with {} holds no earlier run (no *.finder-run.json and no *.cache.json)",
            folder.display()
        ));
    }
    Ok(out)
}

/// The operator note for the difference between an earlier run and this one. The two functions
/// are build 4's comparison ([`run_diff::diff_runs`], [`run_diff::operator_note`]); they are
/// parameters so a test can stand in for them.
pub fn comparison_note(
    earlier: &[Finding],
    current: &[Finding],
    diff: &dyn Fn(&[Finding], &[Finding]) -> RunDiff,
    note: &dyn Fn(&RunDiff) -> String,
) -> String {
    note(&diff(earlier, current))
}

/// The comparison with the real functions.
pub fn compare_with_earlier_run(folder: &Path, current: &[Finding]) -> Result<String, String> {
    let earlier = read_state_folder(folder)?;
    Ok(comparison_note(
        &earlier,
        current,
        &run_diff::diff_runs,
        &run_diff::operator_note,
    ))
}

/// Where the comparison note is written: beside the run's ledger.
pub fn comparison_note_path(ledger_file: &Path) -> PathBuf {
    let stem = ledger_file
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".ledger.json"))
        .unwrap_or("run");
    ledger_file.with_file_name(format!("{stem}.run-difference.md"))
}

/// Build the run's piece agent from the run's backend and the saved key. `Err` before anything
/// is spent when the API backend has no key. The command line transport writes its rooms under
/// `rooms_dir`, an engine-owned folder, never inside the client's repository.
pub fn build_agent(
    controls: &camerata_server::cost_cap::RunControls,
    backend: camerata_server::llm::Backend,
    api_key: Option<&str>,
    rooms_dir: &Path,
) -> Result<Arc<dyn PieceAgent>, String> {
    // FLAG: this meter is the agent's own, not the scan run's shared meter (which is built inside
    // the audit call). An API agent built here counts its spend on this meter only; see the
    // build report. The production path (`find_repo_for_run`) builds its agent with the run's
    // meter.
    let meter = Arc::new(camerata_server::ai_audit::UsageMeter::with_controls(
        controls.clone(),
    ));
    camerata_server::ai_audit::agent_finder::piece_agent_for(
        controls, backend, api_key, rooms_dir, meter,
    )
    .map(Arc::from)
    .map_err(|e| e.to_string())
}
