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
    let record = read_record_at(request.run_dir)?;
    let run_id = record.run_id.clone();
    if record.status != RunStatus::Complete {
        return Err(ImportRefusal::Incomplete {
            status: match record.status {
                RunStatus::Running => "it is marked running".to_string(),
                RunStatus::Incomplete => "a part failed or did not run".to_string(),
                RunStatus::Complete => unreachable!("checked above"),
            },
            reasons: record.incomplete_reasons.clone(),
            run_id,
        });
    }
    let pin = check_pin(&record, request.rule_digest)?;
    let report = read_report(&run_id, request.run_dir)?;
    check_report_is_the_runs(&record, pin, &report)?;
    if report.gated || report.blocked || report.provenance.audited_refs.is_empty() {
        return Err(ImportRefusal::NotAnInspection { run_id });
    }
    // Importing twice changes nothing (and a run goes into one project only).
    if let Some(found) = state.inspections.find_run(&run_id) {
        if request
            .project_id
            .is_some_and(|named| named != found.project_id)
        {
            return Err(ImportRefusal::ImportedElsewhere {
                run_id,
                project_id: found.project_id,
                entry_id: found.id,
            });
        }
        let project_name = state
            .projects
            .get(&found.project_id)
            .map(|p| p.name)
            .unwrap_or_default();
        return Ok(Imported {
            project_id: found.project_id,
            project_name,
            entry_id: found.id,
            run_id,
            already_imported: true,
            findings: found.report.findings.len(),
        });
    }
    let project = resolve_project(state, &run_id, &report.repos, request.project_id)?;

    // The app's own store path, the steps an app scan goes through.
    let report = state.prepare_full_inspection(&project.id, report);
    let stored = state
        .try_record_inspection(
            &project.id,
            &report,
            crate::inspections::InspectionKind::Full,
        )
        .map_err(|e| ImportRefusal::StoreWriteFailed {
            run_id: run_id.clone(),
            why: e.to_string(),
        })?
        .ok_or_else(|| ImportRefusal::NotAnInspection {
            run_id: run_id.clone(),
        })?;
    state.set_last_scan_in_memory(project.id.clone(), report);
    Ok(Imported {
        project_id: project.id,
        project_name: project.name,
        entry_id: stored.id,
        run_id,
        already_imported: false,
        findings: stored.report.findings.len(),
    })
}

/// The run's record, read without taking its lock or writing anything.
fn read_record_at(run_dir: &Path) -> Result<RunRecord, ImportRefusal> {
    let run_id = run_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let path = run_dir.join("run.json");
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ImportRefusal::RunNotFound {
                run_id,
                dir: run_dir.display().to_string(),
            })
        }
        Err(e) => {
            return Err(ImportRefusal::RunUnreadable {
                run_id,
                why: format!("{} could not be read ({e})", path.display()),
            })
        }
    };
    let record: RunRecord =
        serde_json::from_slice(&bytes).map_err(|e| ImportRefusal::RunUnreadable {
            run_id: run_id.clone(),
            why: format!(
                "{} does not read back as a run record ({e})",
                path.display()
            ),
        })?;
    if record.schema != STORE_SCHEMA_VERSION {
        return Err(ImportRefusal::RunUnreadable {
            run_id,
            why: format!(
                "it was saved by another version of Camerata (layout {} instead of {})",
                record.schema, STORE_SCHEMA_VERSION
            ),
        });
    }
    Ok(record)
}

/// The run's pin, when it names its build and its rule library is the one loaded now.
fn check_pin<'r>(
    record: &'r RunRecord,
    rule_digest: &str,
) -> Result<&'r crate::run_store::engine_pin::EnginePin, ImportRefusal> {
    let run_id = || record.run_id.clone();
    let Some(pin) = record.pin.as_ref() else {
        return Err(ImportRefusal::PinMissing {
            run_id: run_id(),
            why: "it recorded no engine pin (it was started before Camerata recorded which build \
                  ran it)"
                .to_string(),
        });
    };
    if pin
        .build
        .commit
        .as_deref()
        .is_none_or(|c| c.trim().is_empty())
    {
        return Err(ImportRefusal::PinMissing {
            run_id: run_id(),
            why: "its engine pin did not record its commit".to_string(),
        });
    }
    if pin.corpus_version.trim().is_empty() || pin.corpus_version != rule_digest {
        return Err(ImportRefusal::RuleDigestMismatch {
            run_id: run_id(),
            pinned: if pin.corpus_version.trim().is_empty() {
                "(none recorded)".to_string()
            } else {
                pin.corpus_version.clone()
            },
            loaded: rule_digest.to_string(),
        });
    }
    Ok(pin)
}

/// The run's saved report, read whole.
fn read_report(run_id: &str, run_dir: &Path) -> Result<ScanReport, ImportRefusal> {
    let path = run_dir.join(REPORT_FILE);
    let shown = path.display().to_string();
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ImportRefusal::ReportMissing {
                run_id: run_id.to_string(),
                path: shown,
            })
        }
        Err(e) => {
            return Err(ImportRefusal::ReportUnreadable {
                run_id: run_id.to_string(),
                path: shown,
                why: e.to_string(),
            })
        }
    };
    serde_json::from_slice(&bytes).map_err(|e| ImportRefusal::ReportUnreadable {
        run_id: run_id.to_string(),
        path: shown,
        why: format!("it does not read back as a report ({e})"),
    })
}

/// The report must be the one the run's pinned engine made for this run.
fn check_report_is_the_runs(
    record: &RunRecord,
    pin: &crate::run_store::engine_pin::EnginePin,
    report: &ScanReport,
) -> Result<(), ImportRefusal> {
    let mismatch = |why: String| {
        Err(ImportRefusal::EnginePinMismatch {
            run_id: record.run_id.clone(),
            why,
        })
    };
    if report.run_id.as_deref() != Some(record.run_id.as_str()) {
        return mismatch(format!(
            "the report names run {}",
            report.run_id.as_deref().unwrap_or("(none)")
        ));
    }
    if report.provenance.camerata_version != pin.build.version {
        return mismatch(format!(
            "the report was made by Camerata {} and the run is pinned to {}",
            report.provenance.camerata_version, pin.build.version
        ));
    }
    if report.provenance.rules_fingerprint != record.rules_fingerprint {
        return mismatch(
            "the report's rule selection is not the one the run was started with".to_string(),
        );
    }
    Ok(())
}

/// The app project the run goes into: the one named, which must hold every repo of the run, or
/// else the one project that does.
fn resolve_project(
    state: &AppState,
    run_id: &str,
    repos: &[String],
    named: Option<&str>,
) -> Result<crate::project::Project, ImportRefusal> {
    let holds = |p: &crate::project::Project| -> Vec<String> {
        repos
            .iter()
            .filter(|r| {
                !p.repos
                    .iter()
                    .any(|have| have.trim().eq_ignore_ascii_case(r.trim()))
            })
            .cloned()
            .collect()
    };
    if let Some(id) = named {
        let project = state
            .projects
            .get(id)
            .ok_or_else(|| ImportRefusal::ProjectUnknown {
                project_id: id.to_string(),
            })?;
        let missing = holds(&project);
        if !missing.is_empty() {
            return Err(ImportRefusal::ProjectReposDiffer {
                run_id: run_id.to_string(),
                project_id: project.id,
                missing,
            });
        }
        return Ok(project);
    }
    let mut candidates: Vec<crate::project::Project> = state
        .projects
        .list()
        .into_iter()
        .filter(|p| holds(p).is_empty())
        .collect();
    match candidates.len() {
        0 => Err(ImportRefusal::NoProject {
            run_id: run_id.to_string(),
            repos: repos.to_vec(),
        }),
        1 => Ok(candidates.remove(0)),
        _ => Err(ImportRefusal::ProjectAmbiguous {
            run_id: run_id.to_string(),
            repos: repos.to_vec(),
            projects: candidates
                .iter()
                .map(|p| format!("{} ({})", p.name, p.id))
                .collect(),
        }),
    }
}

/// The folder of run `run_id` under `runs_root`, refusing an id that is not one folder name (so
/// an id can never reach outside the runs folder).
pub fn run_dir_in(runs_root: &Path, run_id: &str) -> Result<PathBuf, ImportRefusal> {
    let id = run_id.trim();
    if id.is_empty() || id != run_id || id == "." || id == ".." || id.contains(['/', '\\']) {
        return Err(ImportRefusal::InvalidRunId(run_id.to_string()));
    }
    Ok(runs_root.join(id))
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

#[cfg(test)]
mod tests;
