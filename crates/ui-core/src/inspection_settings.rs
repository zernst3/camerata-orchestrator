//! The Inspection settings component's logic (settings overhaul, work order A), with no rendering
//! framework.
//!
//! One component, mounted on the inspection start screen, in Settings > This project and in
//! Review > Settings, holds every setting an inspection reads. Everything it decides lives here,
//! one function per judgment: which rows show for which finder and in what order, each row's
//! label, hint and "(not set)" text, the hidden-row button, the CLI with client engagement
//! conflict line, the read-only "each run" lines and the one-line summary. The screen
//! (`camerata-ui`'s `cockpit/inspection_settings.rs`) only chooses words and pixels.
//!
//! The "(not set: <name>)" text names the built-in default through
//! [`camerata_api_types::project::model_display_name`] of the SAME constant the engine resolves,
//! so the screen cannot drift from the engine.

use camerata_api_types::project::{
    is_anthropic_model_id, model_display_name, ProjectBackend, ProjectFinder, ReviewSettings,
    StepModels, CLONE_JUDGE_DEFAULT_MODEL, DEFAULT_MODEL, DUP_CHECK_DEFAULT_MODEL,
    FINDER_DEFAULT_MODEL, FIX_STAGE_HAIKU_MODEL, STAGE1_DEFAULT_MODEL, STAGE2_DEFAULT_MODEL,
};

use crate::models::{chat_model_groups, ModelOption, ModelsResp};

/// The finder choices the row offers: the word the route takes and what a person reads.
pub const FINDER_CHOICES: [(&str, &str); 2] = [
    ("agent", "Agent (one model reviews each part)"),
    ("engine", "Engine (rule by rule)"),
];

/// One row of the "Finder and models" block, in the order the screen shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Not a model: the project's finder (`agent` or `engine`). It rides the same route.
    Finder,
    /// The agent finder's model.
    FinderModel,
    /// The engine finder's scan model.
    Scan,
    /// The engine finder's stage 1 (calibration) model.
    Calibration,
    /// Stage 2: the group reviewer.
    GroupReview,
    /// The fix stage's model.
    Fix,
    /// The possible-duplicate check's model.
    DuplicateCheck,
    /// The clone judge's model.
    CloneJudge,
}

impl Stage {
    /// Every row in the order the screen shows them.
    pub const ALL: [Stage; 8] = [
        Stage::Finder,
        Stage::FinderModel,
        Stage::Scan,
        Stage::Calibration,
        Stage::GroupReview,
        Stage::Fix,
        Stage::DuplicateCheck,
        Stage::CloneJudge,
    ];

    /// The `step` the step-models route takes for this row.
    pub fn step_key(self) -> &'static str {
        match self {
            Stage::Finder => "finder",
            Stage::FinderModel => "finder_model",
            Stage::Scan => "audit",
            Stage::Calibration => "calibration",
            Stage::GroupReview => "group_review",
            Stage::Fix => "fix",
            Stage::DuplicateCheck => "duplicate_check",
            Stage::CloneJudge => "clone_judge",
        }
    }

    /// The row label.
    pub fn label(self) -> &'static str {
        match self {
            Stage::Finder => "Finder",
            Stage::FinderModel => "Finder model",
            Stage::Scan => "Scan model",
            Stage::Calibration => "Calibration model",
            Stage::GroupReview => "Group review model",
            Stage::Fix => "Fix model",
            Stage::DuplicateCheck => "Duplicate check model",
            Stage::CloneJudge => "Clone judge model",
        }
    }

    /// The built-in default a blank row resolves to, for the rows that may be blank. `None` for
    /// the finder (not a model) and for Scan model and Calibration model, which are always
    /// stored: a blank there is refused by the route.
    pub fn unset_default(self) -> Option<&'static str> {
        match self {
            Stage::FinderModel => Some(FINDER_DEFAULT_MODEL),
            Stage::GroupReview => Some(STAGE2_DEFAULT_MODEL),
            Stage::Fix => Some(FIX_STAGE_HAIKU_MODEL),
            Stage::DuplicateCheck => Some(DUP_CHECK_DEFAULT_MODEL),
            Stage::CloneJudge => Some(CLONE_JUDGE_DEFAULT_MODEL),
            Stage::Finder | Stage::Scan | Stage::Calibration => None,
        }
    }

    /// The first option of a row that can be unset: "(not set: Claude Opus 5.5)".
    pub fn unset_option(self) -> Option<String> {
        self.unset_default()
            .map(|id| format!("(not set: {})", model_display_name(id)))
    }

    /// The one-line hint beside the row.
    pub fn hint(self) -> String {
        match self {
            Stage::Finder => "Decides which model rows below are used.".to_string(),
            Stage::FinderModel => "Reviews each part of the code and writes what it found.".into(),
            Stage::Scan => format!(
                "Engine finder: checks the code rule by rule. Built-in default: {}.",
                model_display_name(DEFAULT_MODEL)
            ),
            Stage::Calibration => format!(
                "Engine finder, stage 1: sets severity and real or not for each place. \
                 Built-in default: {}.",
                model_display_name(STAGE1_DEFAULT_MODEL)
            ),
            Stage::GroupReview => "Stage 2: one call per rule group, real time.".to_string(),
            Stage::Fix => format!(
                "Writes one fix per problem. Not set: {}, or the calibration model when the \
                 Haiku fix switch is off.",
                model_display_name(FIX_STAGE_HAIKU_MODEL)
            ),
            Stage::DuplicateCheck => "Asks whether two write-ups are the same problem.".to_string(),
            Stage::CloneJudge => "Used only when a clone rule is selected.".to_string(),
        }
    }

    /// Which finder uses this row, or `None` when both do.
    pub fn only_for(self) -> Option<ProjectFinder> {
        match self {
            Stage::FinderModel => Some(ProjectFinder::Agent),
            Stage::Scan | Stage::Calibration => Some(ProjectFinder::Engine),
            _ => None,
        }
    }

    /// Whether the finder in effect uses this row.
    pub fn used_by(self, finder: ProjectFinder) -> bool {
        self.only_for().is_none_or(|f| f == finder)
    }
}

/// The word a person reads for a finder, in a sentence.
pub fn finder_word(finder: ProjectFinder) -> &'static str {
    match finder {
        ProjectFinder::Agent => "Agent",
        ProjectFinder::Engine => "Engine",
    }
}

/// Every value the component shows, read from the project.
#[derive(Debug, Clone, PartialEq)]
pub struct InspectionValues {
    pub finder: ProjectFinder,
    pub finder_model: String,
    pub scan: String,
    pub calibration: String,
    pub group_review: String,
    pub fix: String,
    pub duplicate_check: String,
    pub clone_judge: String,
    pub backend: ProjectBackend,
    pub client_engagement: bool,
}

impl InspectionValues {
    /// The values of a project's review settings, step models, backend and engagement flag.
    pub fn of_project(
        review: &ReviewSettings,
        step_models: &StepModels,
        backend: ProjectBackend,
        client_engagement: bool,
    ) -> Self {
        Self {
            finder: review.finder,
            finder_model: review.finder_model.clone(),
            scan: step_models.audit.clone(),
            calibration: step_models.calibration.clone(),
            group_review: review.group_review_model.clone(),
            fix: step_models.fix.clone(),
            duplicate_check: review.dup_check_model.clone(),
            clone_judge: review.clone_judge_model.clone(),
            backend,
            client_engagement,
        }
    }

    /// What the project holds for `stage` (a model id, the finder word, or empty for not set).
    pub fn current(&self, stage: Stage) -> &str {
        match stage {
            Stage::Finder => self.finder.as_str(),
            Stage::FinderModel => &self.finder_model,
            Stage::Scan => &self.scan,
            Stage::Calibration => &self.calibration,
            Stage::GroupReview => &self.group_review,
            Stage::Fix => &self.fix,
            Stage::DuplicateCheck => &self.duplicate_check,
            Stage::CloneJudge => &self.clone_judge,
        }
    }

    /// The model a run would use for `stage`: what is stored, else the built-in default.
    fn effective_model(&self, stage: Stage) -> String {
        let stored = self.current(stage).trim();
        if !stored.is_empty() {
            return stored.to_string();
        }
        stage.unset_default().unwrap_or_default().to_string()
    }
}

/// One row as the screen draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowSpec {
    pub stage: Stage,
    pub label: &'static str,
    pub hint: String,
    /// The stored value: a model id, the finder word, or empty for not set.
    pub current: String,
    /// The first option of a row that can be unset ("(not set: Claude Opus 5.5)").
    pub unset_option: Option<String>,
    /// Set on a row the finder in effect does not use, shown only after the reveal button.
    pub unused_note: Option<String>,
}

/// The rows the screen draws, in order. The rows the finder in effect does not use are left out
/// unless `show_unused` is on, and then they carry [`RowSpec::unused_note`].
pub fn rows(values: &InspectionValues, show_unused: bool) -> Vec<RowSpec> {
    Stage::ALL
        .into_iter()
        .filter_map(|stage| {
            let used = stage.used_by(values.finder);
            if !used && !show_unused {
                return None;
            }
            Some(RowSpec {
                stage,
                label: stage.label(),
                hint: stage.hint(),
                current: values.current(stage).to_string(),
                unset_option: stage.unset_option(),
                unused_note: (!used)
                    .then(|| format!("not used by the {} finder", finder_word(values.finder))),
            })
        })
        .collect()
}

/// The rows the finder in effect does not use.
pub fn unused_rows(finder: ProjectFinder) -> Vec<Stage> {
    Stage::ALL
        .into_iter()
        .filter(|s| !s.used_by(finder))
        .collect()
}

/// The text of the reveal button, or `None` when it is open ("Show the 2 rows the Engine finder
/// uses" for an Agent project, "Show the 1 row the Agent finder uses" for an Engine one).
pub fn unused_rows_button(finder: ProjectFinder, open: bool) -> Option<String> {
    if open {
        return None;
    }
    let n = unused_rows(finder).len();
    let other = match finder {
        ProjectFinder::Agent => ProjectFinder::Engine,
        ProjectFinder::Engine => ProjectFinder::Agent,
    };
    let noun = if n == 1 { "row" } else { "rows" };
    Some(format!(
        "Show the {n} {noun} the {} finder uses",
        finder_word(other)
    ))
}

/// The sentence under the model rows: where a model may come from. It says where models do NOT
/// come from; the component never suggests any other source.
pub const MODELS_NOTE: &str = "Models come from this project, never the environment. The model per stage is recorded in the ledger and disclosed in the methodology section.";

/// The note shown while a run is in flight.
pub const RUN_IN_FLIGHT_NOTE: &str =
    "A run is in flight, so these settings are locked until it finishes.";

/// The conflict line: shown when the backend is CLI and Client engagement is on. A pure function
/// of the two values; the server's pre-flight stays the authority.
pub fn conflict_line(backend: ProjectBackend, client_engagement: bool) -> Option<&'static str> {
    (backend == ProjectBackend::Cli && client_engagement).then_some(
        "A client engagement runs on the API only. Choose API, or turn Client engagement off \
         for an internal run.",
    )
}

/// One line per model row that is set to a non-Anthropic model while Client engagement is on: a
/// client engagement runs only Anthropic models, and only the run-time pre-flight refuses
/// otherwise. Rows the finder in effect does not use are not checked, and an unset row names a
/// built-in Anthropic default.
pub fn engagement_model_conflicts(values: &InspectionValues) -> Vec<String> {
    if !values.client_engagement {
        return Vec::new();
    }
    Stage::ALL
        .into_iter()
        .filter(|s| *s != Stage::Finder && s.used_by(values.finder))
        .filter_map(|stage| {
            let model = values.current(stage).trim();
            (!model.is_empty() && !is_anthropic_model_id(model)).then(|| {
                format!(
                    "This project is a client engagement: only Anthropic models may run. {} is \
                     set to {model}.",
                    stage.label()
                )
            })
        })
        .collect()
}

/// The read-only "Each run" lines: what is chosen per run on the Inspection screen, and the
/// stored escalation numbers that Review > Settings edits once an inspection has finished.
pub fn each_run_lines(finder: ProjectFinder, review: &ReviewSettings) -> Vec<String> {
    let full = match finder {
        ProjectFinder::Agent => "Full inspection: Always. The Agent finder inspects every file.",
        ProjectFinder::Engine => "Full inspection: Chosen per run on the Inspection screen.",
    };
    vec![
        full.to_string(),
        "Spend limit, Checks to run: Chosen per run on the Inspection screen.".to_string(),
        format!(
            "Escalation budget and Engine Check sample: {} per 1,000 lines, floor {}, sample {}. \
             Change them in Review > Settings once an inspection has finished.",
            review.escalation_rate_per_kloc, review.escalation_floor, review.sample_size_per_run
        ),
    ]
}

/// The one-line summary of the recipe, for a collapsed section:
/// "Agent finder, Claude Opus 5.5, group review Claude Opus 5.5, CLI, internal".
pub fn summary_line(values: &InspectionValues) -> String {
    let name = |stage: Stage| model_display_name(&values.effective_model(stage));
    let finder = match values.finder {
        ProjectFinder::Agent => format!("Agent finder, {}", name(Stage::FinderModel)),
        ProjectFinder::Engine => format!(
            "Engine finder, scan {}, calibration {}",
            name(Stage::Scan),
            name(Stage::Calibration)
        ),
    };
    let backend = match values.backend {
        ProjectBackend::Cli => "CLI",
        ProjectBackend::Api => "API",
    };
    let engagement = if values.client_engagement {
        "client engagement"
    } else {
        "internal"
    };
    format!(
        "{finder}, group review {}, {backend}, {engagement}",
        name(Stage::GroupReview)
    )
}

/// The option groups for a row's select: the registry's models, with the row's current model
/// always among them so the select never shows a model that is not there and never renders
/// empty while the registry loads.
pub fn model_groups(models: &Option<ModelsResp>, current: &str) -> Vec<(String, Vec<ModelOption>)> {
    let mut groups = chat_model_groups(models, current);
    let listed = groups
        .iter()
        .any(|(_, options)| options.iter().any(|m| m.id == current));
    if !current.trim().is_empty() && !listed {
        groups.push((
            "Current".to_string(),
            vec![ModelOption {
                label: current.to_string(),
                id: current.to_string(),
                provider: String::new(),
            }],
        ));
    }
    groups
}

#[cfg(test)]
mod tests;
