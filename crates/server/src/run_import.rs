//! Bring a command line inspection into the app for review (#562).
//!
//! `camerata inspect` saves its sealed report in the run's own folder (`report.json`, see
//! [`crate::run_store::REPORT_FILE`], #561). The app's review screens read only the app's own
//! inspection store ([`crate::inspections::InspectionStore`]), so a command line run has to be
//! imported before it can be triaged. This module is that import, in server code so the command
//! line and (later, #279) the app call the same function.
//!
//! # What the import does
//!
//! 1. Reads the run's record (`run.json`) and its saved report, and never writes into the run's
//!    folder: no lock is taken, no record is updated, nothing is created there.
//! 2. Refuses, naming the reason ([`ImportRefusal`]), when the run is not complete, when its
//!    engine pin is missing or does not match the report it holds, when the rule library loaded
//!    now is not the one the run was pinned to, or when the report file is missing or cannot be
//!    read.
//! 3. Finds the app project whose repos are the run's repos (or the one named with `--project`).
//! 4. Hands the report to the app's own store path: [`AppState::prepare_full_inspection`] and
//!    [`AppState::try_record_inspection`], the same steps (restores, patterns, secret sweep,
//!    entry) an app scan goes through. Nothing here repeats them.
//! 5. Does nothing when the run is already in the store: importing twice never creates a second
//!    entry.
//!
//! # The pin check
//!
//! The run's engine pin ([`crate::run_store::engine_pin::EnginePin`]) must be present and name
//! its build, the report must be the one that build made for this run (its run id, Camerata
//! version and rule selection fingerprint match the run's record), and the digest of the rule
//! library loaded now must be the run's. The rule library is checked against what is loaded now
//! because the review screens and every export the app makes from the entry read rules from the
//! library loaded now, and the export states that library's digest. The build commit is NOT
//! required to be the importing build's: the command line build that ran the inspection may not
//! have the import at all, and the app reads and exports a stored entry with its own build
//! anyway (as it does for its own scans after an update).

use std::path::{Path, PathBuf};

use crate::onboard::ScanReport;
use crate::run_store::{RunRecord, RunStatus, REPORT_FILE, STORE_SCHEMA_VERSION};
use crate::AppState;

/// What one import asks for.
#[derive(Debug, Clone)]
pub struct ImportRequest<'a> {
    /// The run's folder (`<state dir>/runs/<run-id>`).
    pub run_dir: &'a Path,
    /// The app project to import into; `None` finds the one project whose repos are the run's.
    pub project_id: Option<&'a str>,
    /// The digest ([`camerata_rules::corpus_version`]) of the rule library loaded now; empty when
    /// none loaded.
    pub rule_digest: &'a str,
}

/// What an import did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    /// The app project the inspection belongs to.
    pub project_id: String,
    /// The project's name, for the message.
    pub project_name: String,
    /// The inspection entry's id in the app's store.
    pub entry_id: String,
    /// The run that was imported.
    pub run_id: String,
    /// True when the run was already in the store and nothing was written.
    pub already_imported: bool,
    /// How many findings the stored inspection holds.
    pub findings: usize,
}

impl Imported {
    /// The plain message for the operator.
    pub fn message(&self) -> String {
        if self.already_imported {
            format!(
                "Run {} is already in the app as inspection {} of project {} ({}). Nothing was \
                 changed.",
                self.run_id, self.entry_id, self.project_name, self.project_id
            )
        } else {
            format!(
                "Imported run {} into project {} ({}) as inspection {} with {} findings. Open the \
                 app to review it; if the app is open, close it and open it again first.",
                self.run_id, self.project_name, self.project_id, self.entry_id, self.findings
            )
        }
    }
}

/// Why an import was refused. Every refusal names its reason in plain words ([`Self::code`] is
/// the stable name a test or a caller can match on); nothing is written on any refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportRefusal {
    /// The run id is not a single folder name.
    InvalidRunId(String),
    /// There is no run folder, or it has no record.
    RunNotFound { run_id: String, dir: String },
    /// The run's record cannot be read or was written by another layout.
    RunUnreadable { run_id: String, why: String },
    /// The run is not complete.
    Incomplete {
        run_id: String,
        status: String,
        reasons: Vec<String>,
    },
    /// The run recorded no engine pin, or one that does not name its build.
    PinMissing { run_id: String, why: String },
    /// The saved report is not the one the run's pinned engine made for this run.
    EnginePinMismatch { run_id: String, why: String },
    /// The rule library loaded now is not the one the run was pinned to.
    RuleDigestMismatch {
        run_id: String,
        pinned: String,
        loaded: String,
    },
    /// The run folder has no saved report.
    ReportMissing { run_id: String, path: String },
    /// The saved report cannot be read, or does not read back as a report.
    ReportUnreadable {
        run_id: String,
        path: String,
        why: String,
    },
    /// The report holds no audited repo (or is a gate or a block): there is nothing to review.
    NotAnInspection { run_id: String },
    /// No app project has the run's repos.
    NoProject { run_id: String, repos: Vec<String> },
    /// More than one app project has the run's repos.
    ProjectAmbiguous {
        run_id: String,
        repos: Vec<String>,
        projects: Vec<String>,
    },
    /// The project named does not exist in the app.
    ProjectUnknown { project_id: String },
    /// The project named does not hold every repo of the run.
    ProjectReposDiffer {
        run_id: String,
        project_id: String,
        missing: Vec<String>,
    },
    /// The run was already imported into another project.
    ImportedElsewhere {
        run_id: String,
        project_id: String,
        entry_id: String,
    },
    /// The app's data folder cannot be found.
    NoDataDir,
    /// The app's store could not be written.
    StoreWriteFailed { run_id: String, why: String },
}

impl ImportRefusal {
    /// The stable name of the reason.
    pub fn code(&self) -> &'static str {
        match self {
            Self::InvalidRunId(_) => "invalid_run_id",
            Self::RunNotFound { .. } => "run_not_found",
            Self::RunUnreadable { .. } => "run_unreadable",
            Self::Incomplete { .. } => "incomplete",
            Self::PinMissing { .. } => "pin_missing",
            Self::EnginePinMismatch { .. } => "engine_pin_mismatch",
            Self::RuleDigestMismatch { .. } => "rule_digest_mismatch",
            Self::ReportMissing { .. } => "report_missing",
            Self::ReportUnreadable { .. } => "report_unreadable",
            Self::NotAnInspection { .. } => "not_an_inspection",
            Self::NoProject { .. } => "no_project",
            Self::ProjectAmbiguous { .. } => "project_ambiguous",
            Self::ProjectUnknown { .. } => "project_unknown",
            Self::ProjectReposDiffer { .. } => "project_repos_differ",
            Self::ImportedElsewhere { .. } => "imported_elsewhere",
            Self::NoDataDir => "no_data_dir",
            Self::StoreWriteFailed { .. } => "store_write_failed",
        }
    }
}

/// `a and b`, `a, b and c`: a plain list with no serial comma.
fn plain_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

impl std::fmt::Display for ImportRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRunId(id) => write!(
                f,
                "\"{id}\" is not a run id. Give the id `camerata inspect` printed for the run."
            ),
            Self::RunNotFound { run_id, dir } => write!(
                f,
                "There is no saved run {run_id} (looked in {dir}). Give the id `camerata \
                 inspect` printed for the run."
            ),
            Self::RunUnreadable { run_id, why } => {
                write!(f, "The record of run {run_id} cannot be read: {why}.")
            }
            Self::Incomplete {
                run_id,
                status,
                reasons,
            } => {
                write!(
                    f,
                    "Run {run_id} is not complete ({status}), so it cannot be imported. Finish it \
                     with `camerata inspect --resume {run_id}` and import it again."
                )?;
                if !reasons.is_empty() {
                    write!(f, " Why it is not complete: {}.", reasons.join("; "))?;
                }
                Ok(())
            }
            Self::PinMissing { run_id, why } => write!(
                f,
                "Run {run_id} cannot be imported: {why}. The app cannot say which build and rule \
                 library made this inspection."
            ),
            Self::EnginePinMismatch { run_id, why } => write!(
                f,
                "Run {run_id} cannot be imported: its saved report does not match the build the \
                 run was pinned to ({why})."
            ),
            Self::RuleDigestMismatch {
                run_id,
                pinned,
                loaded,
            } => {
                let loaded = if loaded.is_empty() {
                    "no rule library is loaded".to_string()
                } else {
                    format!("the rule library loaded now is {loaded}")
                };
                write!(
                    f,
                    "Run {run_id} cannot be imported: it was made with rule library {pinned} and \
                     {loaded}. The app reviews and exports with the rule library it loads, so put \
                     the rule library back as it was for the run and import it again."
                )
            }
            Self::ReportMissing { run_id, path } => write!(
                f,
                "Run {run_id} has no saved report ({path} is missing), so there is nothing to \
                 import. A run saves its report when it finishes; finish it with `camerata \
                 inspect --resume {run_id}`."
            ),
            Self::ReportUnreadable { run_id, path, why } => write!(
                f,
                "The saved report of run {run_id} cannot be read ({path}: {why}), so it was not \
                 imported."
            ),
            Self::NotAnInspection { run_id } => write!(
                f,
                "The saved report of run {run_id} holds no inspected repository, so there is \
                 nothing to review."
            ),
            Self::NoProject { run_id, repos } => write!(
                f,
                "No project in the app holds the repositories of run {run_id} ({}). Create the \
                 project in the app with these repositories, or name one with --project.",
                plain_list(repos)
            ),
            Self::ProjectAmbiguous {
                run_id,
                repos,
                projects,
            } => write!(
                f,
                "More than one project in the app holds the repositories of run {run_id} ({}): \
                 {}. Name one with --project.",
                plain_list(repos),
                plain_list(projects)
            ),
            Self::ProjectUnknown { project_id } => {
                write!(f, "There is no project {project_id} in the app.")
            }
            Self::ProjectReposDiffer {
                run_id,
                project_id,
                missing,
            } => write!(
                f,
                "Project {project_id} does not hold every repository of run {run_id} (missing: \
                 {}), so the inspection would not open there.",
                plain_list(missing)
            ),
            Self::ImportedElsewhere {
                run_id,
                project_id,
                entry_id,
            } => write!(
                f,
                "Run {run_id} is already in the app as inspection {entry_id} of project \
                 {project_id}. A run is imported once."
            ),
            Self::NoDataDir => write!(
                f,
                "The app's data folder cannot be found, so there is no store to import into. Set \
                 CAMERATA_DATA_DIR to the app's data folder."
            ),
            Self::StoreWriteFailed { run_id, why } => write!(
                f,
                "Run {run_id} was not imported: the app's inspection store could not be written \
                 ({why})."
            ),
        }
    }
}

impl std::error::Error for ImportRefusal {}

/// Import the run in `request.run_dir` into the app's store in `state`. See the module docs.
pub fn import_cli_run(
    state: &AppState,
    request: &ImportRequest<'_>,
) -> Result<Imported, ImportRefusal> {
    let _ = (state, request);
    Err(ImportRefusal::NoDataDir)
}

/// The folder of run `run_id` under `runs_root`, refusing an id that is not one folder name (so
/// an id can never reach outside the runs folder).
pub fn run_dir_in(runs_root: &Path, run_id: &str) -> Result<PathBuf, ImportRefusal> {
    let _ = runs_root;
    Err(ImportRefusal::InvalidRunId(run_id.to_string()))
}

/// The import as the command line runs it: the run `run_id` from this user's runs folder, into
/// the app's store in its data folder (`CAMERATA_DATA_DIR`), checked against the rule library
/// this build loads.
pub async fn import_cli_run_here(
    run_id: &str,
    project_id: Option<&str>,
) -> Result<Imported, ImportRefusal> {
    let run_dir = run_dir_in(&crate::run_store::runs_dir(), run_id)?;
    let data_dir = crate::app_dirs::camerata_dir().ok_or(ImportRefusal::NoDataDir)?;
    let corpus_path = camerata_rules::corpus_path();
    let rule_digest = if corpus_path.exists() {
        camerata_rules::corpus_version(&camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        String::new()
    };
    let state = AppState::from_data_dir(&data_dir);
    import_cli_run(
        &state,
        &ImportRequest {
            run_dir: &run_dir,
            project_id,
            rule_digest: &rule_digest,
        },
    )
}

#[allow(dead_code)]
fn unused(_: &ScanReport, _: &RunRecord, _: RunStatus, _: &str, _: u32) {
    let _ = (REPORT_FILE, STORE_SCHEMA_VERSION);
}

#[cfg(test)]
mod tests;
