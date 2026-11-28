//! The client report's list of model stages and the model each ran on (#584).
//!
//! Built once, here, from the run's own record ([`crate::scan_ledger::ScanLedger::model_stages`],
//! or for a run saved before that record existed, the single-model fields of
//! [`crate::onboard::ScanProvenance`]). Never from today's defaults or settings: a re-export of a
//! saved run names what that run ran on. The PDF data (`cover.model_stages`), the workbook's Start
//! here sheet and `json/inspection.json`'s `provenance` all carry the value this returns, so the
//! three cannot disagree.
//!
//! A stage that made no model call is left off. A stage the record shows ran but whose model it
//! did not save says "model not recorded"; no stage is ever given a default model name.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::stage_models::RecordedStageModels;
use crate::stage_telemetry::{Stage, MODE_BATCH, MODE_LIVE};

/// One model stage as the client report prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelStageJson {
    /// A stable key for machine readers: `finder`, `group_review`, `duplicate_check`,
    /// `clone_check`, `fix_text`, `calibration`, `finding_detail`, `alternatives`, `deep_review`.
    pub stage: &'static str,
    /// The stage's name in the report's words.
    pub label: String,
    /// `used` when the record names the model, `not_recorded` when the record shows the stage ran
    /// but did not save its model.
    pub status: &'static str,
    /// The model ids the stage's calls ran on, in id order. A CLI call's id carries
    /// ` (requested)`: the CLI says which model was asked for, not which one answered. Empty when
    /// not recorded.
    pub models: Vec<String>,
    /// `batch`, `live` or `batch and live`. `None` when the record does not say.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<&'static str>,
    /// For the fix text: how many findings ended on the rule's standard fix instead of a fix
    /// written for the codebase. `None` for every other stage, and when not recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standard_fix_fallbacks: Option<u64>,
    /// What the workbook prints beside the label: the models, the mode and the fallbacks.
    pub detail: String,
    /// The whole line the PDF prints: `label: detail.`
    pub line: String,
}

/// Every model stage of the run, and the note that goes under the list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ModelStagesJson {
    pub stages: Vec<ModelStageJson>,
    /// Printed under the list: that no model was used, or that the run was saved before each
    /// stage's model was recorded. `None` otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// The note under the list of a run saved before each stage's model was recorded.
pub const OLDER_RUN_NOTE: &str = "This inspection was saved before the model of each stage was \
     recorded. A stage that says model not recorded ran, but its model was not saved with the run.";

/// The note of a run that called no model but carried its model findings from an earlier
/// inspection (an incremental run whose files had not changed).
pub const CARRIED_ALL_NOTE: &str = "No model was called in this inspection. Its model findings \
     were carried from an earlier inspection. That inspection's models are not listed here.";

/// The note of a run that called models and also carried model findings from an earlier
/// inspection.
pub const CARRIED_SOME_NOTE: &str = "Some model findings were carried from an earlier \
     inspection. This list names only the models this inspection called.";

/// The note of a run that made no model call.
pub const NO_MODEL_NOTE: &str = "No model was used in this inspection.";

/// The run's model stages, from its own record. See the module doc.
pub fn model_stages(report: &crate::onboard::ScanReport) -> ModelStagesJson {
    let multi_repo = report.repos.len() > 1;
    // An incremental run carries the earlier inspection's model findings for the files that did
    // not change: those findings came from a model this run did not call.
    let carried = report
        .ledger
        .incremental()
        .is_some_and(|c| c.carried_findings > 0);
    match report.ledger.model_stages() {
        Some(record) if !record.partial => {
            let stages = from_record(record, multi_repo);
            let note = match (stages.is_empty(), carried) {
                (true, true) => Some(CARRIED_ALL_NOTE),
                (true, false) => Some(NO_MODEL_NOTE),
                (false, true) => Some(CARRIED_SOME_NOTE),
                (false, false) => None,
            };
            ModelStagesJson {
                stages,
                note: note.map(str::to_string),
            }
        }
        later => from_older_run(report, later, multi_repo, carried),
    }
}

/// A model stage as the client reads it. The finder is the rule scan passes and the agent finder
/// together (its joint checks across repositories are agent finder turns).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientStage {
    Finder,
    GroupReview,
    DuplicateCheck,
    CloneCheck,
    FixText,
    Calibration,
    FindingDetail,
    Alternatives,
    DeepReview,
}

impl ClientStage {
    /// The order the list prints.
    const ALL: [ClientStage; 9] = [
        ClientStage::Finder,
        ClientStage::GroupReview,
        ClientStage::DuplicateCheck,
        ClientStage::CloneCheck,
        ClientStage::FixText,
        ClientStage::Calibration,
        ClientStage::FindingDetail,
        ClientStage::Alternatives,
        ClientStage::DeepReview,
    ];

    fn key(self) -> &'static str {
        match self {
            ClientStage::Finder => "finder",
            ClientStage::GroupReview => "group_review",
            ClientStage::DuplicateCheck => "duplicate_check",
            ClientStage::CloneCheck => "clone_check",
            ClientStage::FixText => "fix_text",
            ClientStage::Calibration => "calibration",
            ClientStage::FindingDetail => "finding_detail",
            ClientStage::Alternatives => "alternatives",
            ClientStage::DeepReview => "deep_review",
        }
    }

    fn label(self, multi_repo: bool) -> &'static str {
        match self {
            ClientStage::Finder if multi_repo => "Finder and the checks across repositories",
            ClientStage::Finder => "Finder",
            ClientStage::GroupReview => "Group review",
            ClientStage::DuplicateCheck => "Duplicate check",
            ClientStage::CloneCheck => "Clone check",
            ClientStage::FixText => "Fix text",
            ClientStage::Calibration => "Calibration",
            ClientStage::FindingDetail => "Finding detail",
            ClientStage::Alternatives => "Alternative options",
            ClientStage::DeepReview => "Deep compliance and security review",
        }
    }

    /// The pipeline stages whose calls this client stage is made of.
    fn stages(self) -> &'static [Stage] {
        match self {
            ClientStage::Finder => &[Stage::ScanPass, Stage::Finder],
            ClientStage::GroupReview => &[Stage::GroupReview],
            ClientStage::DuplicateCheck => &[Stage::DuplicateCheck],
            ClientStage::CloneCheck => &[Stage::CloneJudge],
            ClientStage::FixText => &[Stage::FixGeneration],
            ClientStage::Calibration => &[Stage::Calibration],
            ClientStage::FindingDetail => &[Stage::DetailRegeneration],
            ClientStage::Alternatives => &[Stage::Alternatives],
            ClientStage::DeepReview => &[Stage::DeepTier],
        }
    }
}

/// The list from a run that recorded its model stages: every client stage with at least one call.
fn from_record(record: &RecordedStageModels, multi_repo: bool) -> Vec<ModelStageJson> {
    let mut out = Vec::new();
    for stage in ClientStage::ALL {
        let records: Vec<_> = stage
            .stages()
            .iter()
            .filter_map(|s| record.stage(*s))
            .collect();
        if records.is_empty() {
            continue;
        }
        let mut models: BTreeMap<&str, u64> = BTreeMap::new();
        let mut modes: BTreeMap<&str, u64> = BTreeMap::new();
        for r in &records {
            for (model, calls) in &r.models {
                *models.entry(model.as_str()).or_default() += calls;
            }
            for (mode, calls) in &r.modes {
                *modes.entry(mode.as_str()).or_default() += calls;
            }
        }
        if models.is_empty() {
            out.push(not_recorded(stage, multi_repo));
            continue;
        }
        // A part of the stage copied from telemetry that predates the mode count leaves the mode
        // unsaid rather than half said.
        let mode = if records.iter().all(|r| !r.modes.is_empty()) {
            mode_words(&modes)
        } else {
            None
        };
        let fallbacks = if stage == ClientStage::FixText {
            record.standard_fix_fallbacks
        } else {
            None
        };
        out.push(used(
            stage,
            multi_repo,
            models.keys().map(|m| (*m).to_string()).collect(),
            mode,
            fallbacks,
        ));
    }
    out
}

/// The list from a run saved before its model stages were recorded: the stages the run's
/// provenance names a model for, and "model not recorded" for each stage the run shows it ran
/// without naming the model. A stage the saved run gives no sign of is left off.
fn from_older_run(
    report: &crate::onboard::ScanReport,
    later: Option<&RecordedStageModels>,
    multi_repo: bool,
    carried: bool,
) -> ModelStagesJson {
    let p = &report.provenance;
    let named = |model: &Option<String>| -> Option<String> {
        model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(str::to_string)
    };
    // The stages a newer build ran on this run later (a partial record): each names its model.
    let later = later.map_or_else(Vec::new, |r| from_record(r, multi_repo));
    // Provenance stamps the configured calibration and group review models whether or not the
    // stage ran, so either is listed only when the run's stage record shows its calls. Only the
    // call count is read there, never a model or a figure.
    let ran = |stage: Stage| {
        report.ledger.stage_telemetry().is_some_and(|t| {
            t.stages
                .iter()
                .any(|s| s.stage == stage && s.usage.calls > 0)
        })
    };
    // `audit_model` is stamped only when a model call was made, so without it the scan made none.
    let scan_model = named(&p.audit_model);
    if scan_model.is_none() && later.is_empty() {
        let note = if carried {
            CARRIED_ALL_NOTE
        } else {
            NO_MODEL_NOTE
        };
        return ModelStagesJson {
            stages: Vec::new(),
            note: Some(note.to_string()),
        };
    }
    let mut stages = Vec::new();
    for stage in ClientStage::ALL {
        if let Some(entry) = later.iter().find(|e| e.stage == stage.key()) {
            stages.push(entry.clone());
            continue;
        }
        // `Some(Some(model))`: ran on a named model; `Some(None)`: ran, model not saved.
        let entry: Option<Option<String>> = match stage {
            ClientStage::Finder => scan_model.clone().map(Some),
            ClientStage::GroupReview => named(&p.group_review_model)
                .filter(|_| ran(Stage::GroupReview))
                .map(Some),
            ClientStage::CloneCheck => named(&p.clone_judge_model).map(Some),
            ClientStage::Calibration => named(&p.calibration_model)
                .filter(|_| ran(Stage::Calibration))
                .map(Some),
            // Each stored pair is a model's answer.
            ClientStage::DuplicateCheck => {
                (!report.ledger.duplicate_checks().pairs.is_empty()).then_some(None)
            }
            // A fix written for the codebase is the fix stage's output.
            ClientStage::FixText => report
                .findings
                .iter()
                .any(|f| f.fix_specific.is_some())
                .then_some(None),
            ClientStage::DeepReview => p.deep.then_some(None),
            // An older run keeps no sign of these two stages.
            ClientStage::FindingDetail | ClientStage::Alternatives => None,
        };
        match entry {
            Some(Some(model)) => stages.push(used(stage, multi_repo, vec![model], None, None)),
            Some(None) => stages.push(not_recorded(stage, multi_repo)),
            None => {}
        }
    }
    let note = if carried {
        format!("{OLDER_RUN_NOTE} {CARRIED_SOME_NOTE}")
    } else {
        OLDER_RUN_NOTE.to_string()
    };
    ModelStagesJson {
        stages,
        note: Some(note),
    }
}

/// `batch`, `live` or `batch and live`, from a stage's calls per mode.
fn mode_words(modes: &BTreeMap<&str, u64>) -> Option<&'static str> {
    let has = |mode: &str| modes.get(mode).is_some_and(|n| *n > 0);
    match (has(MODE_BATCH), has(MODE_LIVE)) {
        (true, true) => Some("batch and live"),
        (true, false) => Some("batch"),
        (false, true) => Some("live"),
        (false, false) => None,
    }
}

/// `a`, `a and b`, `a, b and c`: a list with no serial comma.
fn plain_list(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

fn used(
    stage: ClientStage,
    multi_repo: bool,
    models: Vec<String>,
    mode: Option<&'static str>,
    standard_fix_fallbacks: Option<u64>,
) -> ModelStageJson {
    let mut detail = plain_list(&models);
    if let Some(mode) = mode {
        detail.push_str(", ");
        detail.push_str(mode);
    }
    match standard_fix_fallbacks {
        Some(1) => detail.push_str(". 1 finding uses the rule's standard fix"),
        Some(n) if n > 1 => {
            detail.push_str(&format!(". {n} findings use the rule's standard fix"));
        }
        _ => {}
    }
    entry(
        stage,
        multi_repo,
        "used",
        models,
        mode,
        standard_fix_fallbacks,
        detail,
    )
}

fn not_recorded(stage: ClientStage, multi_repo: bool) -> ModelStageJson {
    entry(
        stage,
        multi_repo,
        "not_recorded",
        Vec::new(),
        None,
        None,
        "model not recorded".to_string(),
    )
}

#[allow(clippy::too_many_arguments)]
fn entry(
    stage: ClientStage,
    multi_repo: bool,
    status: &'static str,
    models: Vec<String>,
    mode: Option<&'static str>,
    standard_fix_fallbacks: Option<u64>,
    detail: String,
) -> ModelStageJson {
    let label = stage.label(multi_repo).to_string();
    ModelStageJson {
        stage: stage.key(),
        line: format!("{label}: {detail}."),
        label,
        status,
        models,
        mode,
        standard_fix_fallbacks,
        detail,
    }
}

#[cfg(test)]
#[path = "model_stages_tests.rs"]
mod tests;
