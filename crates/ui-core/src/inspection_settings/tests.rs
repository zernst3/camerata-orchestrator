use super::*;
use camerata_api_types::project::{ReviewSettings, StepModels};

fn values(finder: ProjectFinder) -> InspectionValues {
    let review = ReviewSettings {
        finder,
        ..ReviewSettings::for_new_project()
    };
    InspectionValues::of_project(&review, &StepModels::default(), ProjectBackend::Api, true)
}

fn order(rows: &[RowSpec]) -> Vec<&'static str> {
    rows.iter().map(|r| r.stage.label()).collect()
}

#[test]
fn the_agent_finder_shows_its_rows_in_the_designed_order_and_hides_scan_and_calibration() {
    let shown = rows(&values(ProjectFinder::Agent), false);
    assert_eq!(
        order(&shown),
        [
            "Finder",
            "Finder model",
            "Group review model",
            "Fix model",
            "Duplicate check model",
            "Clone judge model"
        ]
    );
}

#[test]
fn the_engine_finder_shows_scan_and_calibration_and_hides_the_finder_model() {
    let shown = rows(&values(ProjectFinder::Engine), false);
    assert_eq!(
        order(&shown),
        [
            "Finder",
            "Scan model",
            "Calibration model",
            "Group review model",
            "Fix model",
            "Duplicate check model",
            "Clone judge model"
        ]
    );
}

#[test]
fn the_reveal_button_counts_the_hidden_rows_and_the_revealed_rows_are_marked() {
    assert_eq!(
        unused_rows_button(ProjectFinder::Agent, false).as_deref(),
        Some("Show the 2 rows the Engine finder uses")
    );
    assert_eq!(
        unused_rows_button(ProjectFinder::Engine, false).as_deref(),
        Some("Show the 1 row the Agent finder uses")
    );
    assert_eq!(unused_rows_button(ProjectFinder::Agent, true), None);
    let all = rows(&values(ProjectFinder::Agent), true);
    assert_eq!(all.len(), 8);
    let unused: Vec<_> = all.iter().filter(|r| r.unused_note.is_some()).collect();
    assert_eq!(order(&all)[2..4], ["Scan model", "Calibration model"]);
    assert_eq!(unused.len(), 2);
    assert_eq!(
        unused[0].unused_note.as_deref(),
        Some("not used by the Agent finder")
    );
    let engine = rows(&values(ProjectFinder::Engine), true);
    assert_eq!(
        engine
            .iter()
            .find(|r| r.unused_note.is_some())
            .and_then(|r| r.unused_note.as_deref()),
        Some("not used by the Engine finder")
    );
}

#[test]
fn every_unset_row_names_the_built_in_default_from_the_constant_the_engine_resolves() {
    for (stage, constant) in [
        (Stage::FinderModel, FINDER_DEFAULT_MODEL),
        (Stage::GroupReview, STAGE2_DEFAULT_MODEL),
        (Stage::Fix, FIX_STAGE_HAIKU_MODEL),
        (Stage::DuplicateCheck, DUP_CHECK_DEFAULT_MODEL),
        (Stage::CloneJudge, CLONE_JUDGE_DEFAULT_MODEL),
    ] {
        assert_eq!(
            stage.unset_option(),
            Some(format!("(not set: {})", model_display_name(constant))),
            "{stage:?}"
        );
    }
    assert_eq!(
        Stage::Fix.unset_option().as_deref(),
        Some("(not set: Claude Haiku 5.5)")
    );
    assert_eq!(
        Stage::CloneJudge.unset_option().as_deref(),
        Some("(not set: Claude Haiku 5.5)")
    );
}

#[test]
fn scan_and_calibration_stay_always_stored_and_name_their_default_in_the_hint() {
    assert_eq!(Stage::Scan.unset_option(), None);
    assert_eq!(Stage::Calibration.unset_option(), None);
    assert_eq!(Stage::Finder.unset_option(), None);
    assert!(Stage::Scan
        .hint()
        .ends_with("Built-in default: Claude Sonnet 5.5."));
    assert!(Stage::Calibration
        .hint()
        .ends_with("Built-in default: Claude Opus 5.5."));
}

#[test]
fn each_row_names_the_step_the_route_takes_and_reads_its_own_saved_value() {
    let keys: Vec<&str> = Stage::ALL.iter().map(|s| s.step_key()).collect();
    assert_eq!(
        keys,
        [
            "finder",
            "finder_model",
            "audit",
            "calibration",
            "group_review",
            "fix",
            "duplicate_check",
            "clone_judge"
        ]
    );
    let v = InspectionValues {
        finder: ProjectFinder::Engine,
        finder_model: "a".into(),
        scan: "b".into(),
        calibration: "c".into(),
        group_review: "d".into(),
        fix: String::new(),
        duplicate_check: "e".into(),
        clone_judge: String::new(),
        backend: ProjectBackend::Cli,
        client_engagement: false,
    };
    let current: Vec<&str> = Stage::ALL.iter().map(|s| v.current(*s)).collect();
    assert_eq!(current, ["engine", "a", "b", "c", "d", "", "e", ""]);
}

#[test]
fn the_conflict_line_appears_only_for_cli_with_client_engagement_on() {
    assert!(conflict_line(ProjectBackend::Cli, true).is_some());
    assert!(conflict_line(ProjectBackend::Cli, false).is_none());
    assert!(conflict_line(ProjectBackend::Api, true).is_none());
    assert!(conflict_line(ProjectBackend::Api, false).is_none());
    assert_eq!(
        conflict_line(ProjectBackend::Cli, true),
        Some(
            "A client engagement runs on the API only. Choose API, or turn Client engagement off \
             for an internal run."
        )
    );
}

#[test]
fn the_summary_line_for_the_recipe_reads_as_the_design_says() {
    let mut v = values(ProjectFinder::Agent);
    v.backend = ProjectBackend::Cli;
    v.client_engagement = false;
    assert_eq!(
        summary_line(&v),
        "Agent finder, Claude Opus 5.5, group review Claude Opus 5.5, CLI, internal"
    );
    let engine = values(ProjectFinder::Engine);
    assert_eq!(
        summary_line(&engine),
        "Engine finder, scan Claude Sonnet 5.5, calibration Claude Opus 5.5, group review \
         Claude Opus 5.5, API, client engagement"
    );
}

#[test]
fn the_each_run_lines_follow_the_finder_and_quote_the_stored_numbers() {
    let review = ReviewSettings::for_new_project();
    let agent = each_run_lines(ProjectFinder::Agent, &review);
    assert!(agent[0].contains("Always. The Agent finder inspects every file."));
    let engine = each_run_lines(ProjectFinder::Engine, &review);
    assert!(engine[0].contains("Chosen per run on the Inspection screen."));
    assert!(agent[2].contains("floor 20, sample 50"), "{}", agent[2]);
}

#[test]
fn no_text_of_the_component_has_a_dash_or_names_the_environment_outside_its_one_sentence() {
    let mut all = vec![
        MODELS_NOTE.to_string(),
        RUN_IN_FLIGHT_NOTE.to_string(),
        conflict_line(ProjectBackend::Cli, true)
            .unwrap()
            .to_string(),
    ];
    for stage in Stage::ALL {
        all.push(stage.hint());
        all.push(stage.label().to_string());
        all.extend(stage.unset_option());
    }
    all.extend(each_run_lines(
        ProjectFinder::Agent,
        &ReviewSettings::for_new_project(),
    ));
    for text in all {
        assert!(
            !text.contains('\u{2014}') && !text.contains('\u{2013}'),
            "{text}"
        );
        let rest = text.replace("never the environment", "").to_lowercase();
        assert!(!rest.contains("environment"), "{text}");
    }
}

fn ids(groups: &[(String, Vec<ModelOption>)]) -> Vec<String> {
    groups
        .iter()
        .flat_map(|(_, o)| o.iter().map(|m| m.id.clone()))
        .collect()
}

#[test]
fn a_select_always_offers_the_model_it_currently_holds() {
    let registry = Some(ModelsResp {
        models: vec![
            ModelOption {
                label: "Opus".into(),
                id: "claude-opus-5-5".into(),
                provider: "claude".into(),
            },
            ModelOption {
                label: "Fable".into(),
                id: "claude-fable-5-1".into(),
                provider: "claude".into(),
            },
        ],
        default: String::new(),
        backend: String::new(),
    });
    assert_eq!(
        ids(&model_groups(&registry, "claude-fable-5-1")),
        ["claude-opus-5-5", "claude-fable-5-1"]
    );
    assert_eq!(
        ids(&model_groups(&registry, "my-own-model")),
        ["claude-opus-5-5", "claude-fable-5-1", "my-own-model"],
        "a model the registry does not list is still shown"
    );
    assert_eq!(
        ids(&model_groups(&registry, "")),
        ["claude-opus-5-5", "claude-fable-5-1"],
        "an unset row adds nothing"
    );
    assert_eq!(
        ids(&model_groups(&None, "claude-opus-5-5")),
        ["claude-opus-5-5"],
        "no registry: the current model alone, never an empty select"
    );
}

#[test]
fn the_fix_hint_is_true_with_the_haiku_switch_on_and_off() {
    let hint = Stage::Fix.hint();
    assert!(hint.contains("Not set: Claude Haiku 5.5"), "{hint}");
    assert!(
        hint.contains("or the calibration model when the Haiku fix switch is off"),
        "{hint}"
    );
}

#[test]
fn a_non_anthropic_model_on_a_client_engagement_is_named_and_only_then() {
    let mut v = values(ProjectFinder::Agent);
    v.client_engagement = true;
    assert!(
        engagement_model_conflicts(&v).is_empty(),
        "unset rows are Anthropic defaults"
    );
    v.group_review = "meta/llama-3:free".into();
    v.scan = "vendor/other".into();
    assert_eq!(
        engagement_model_conflicts(&v),
        ["This project is a client engagement: only Anthropic models may run. Group review model \
          is set to meta/llama-3:free."],
        "Scan model is not used by the Agent finder, so it is not checked"
    );
    v.client_engagement = false;
    assert!(engagement_model_conflicts(&v).is_empty());
    v.client_engagement = true;
    v.finder = ProjectFinder::Engine;
    assert_eq!(engagement_model_conflicts(&v).len(), 2);
}

#[test]
fn a_refused_write_reads_as_one_operator_sentence() {
    assert_eq!(
        save_error_line(Stage::GroupReview, "model must not be empty"),
        "Not saved. Group review model: a model must be chosen. The previous value was kept."
    );
    assert_eq!(
        save_error_line(Stage::Finder, "The finder must be agent or engine."),
        "Not saved. Finder: the finder must be agent or engine. The previous value was kept."
    );
    assert_eq!(
        save_error_line(Stage::Fix, ""),
        "Not saved. Fix model: the server refused the change. The previous value was kept."
    );
}

#[test]
fn with_no_registry_a_blank_model_never_says_loading() {
    let groups = model_groups(&None, "");
    let labels: Vec<&str> = groups
        .iter()
        .flat_map(|(_, o)| o.iter().map(|m| m.label.as_str()))
        .collect();
    assert!(!labels.is_empty(), "never an empty select");
    assert!(labels.iter().all(|l| !l.contains("Loading")), "{labels:?}");
}
