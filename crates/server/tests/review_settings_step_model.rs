#![allow(clippy::unwrap_used)]
//! WO11b: the settings lane's stage 2 model write. Review spec section 9 sends the group review
//! model through the one step-models route (`step: "group_review"`); the project keeps it in its
//! review settings (`review.group_review_model`), so the escalation fields next to it survive.

#[path = "common/review.rs"]
mod review;

use review::{call, review_fixture};
use serde_json::{json, Value};

#[tokio::test]
async fn the_step_models_route_sets_the_group_review_model_and_leaves_the_rest_of_the_settings() {
    let f = review_fixture(6);
    let before = f.state.projects().get(&f.project_id).unwrap();
    let (status, text) = call(
        &f.app,
        "POST",
        &format!("/api/projects/{}/step-models", f.project_id),
        Some(json!({ "step": "group_review", "model": "claude-opus-5-5" })),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    let reply: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(reply["ok"], json!(true), "{text}");
    let after = f.state.projects().get(&f.project_id).unwrap();
    assert_eq!(after.review.group_review_model, "claude-opus-5-5");
    assert_eq!(
        after.review.escalation_floor, before.review.escalation_floor,
        "the escalation fields are untouched"
    );
    assert_eq!(
        after.step_models, before.step_models,
        "the group review model is not one of the per-step models"
    );
}

#[tokio::test]
async fn a_blank_group_review_model_clears_the_setting_and_the_run_resolves_the_built_in_default() {
    let f = review_fixture(6);
    // Start from an explicit pick so the clear is visible.
    let (_, text) = call(
        &f.app,
        "POST",
        &format!("/api/projects/{}/step-models", f.project_id),
        Some(json!({ "step": "group_review", "model": "claude-fable-5-2" })),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["ok"],
        json!(true),
        "{text}"
    );
    let before = f.state.projects().get(&f.project_id).unwrap();
    assert_eq!(before.review.group_review_model, "claude-fable-5-2");
    let (_, text) = call(
        &f.app,
        "POST",
        &format!("/api/projects/{}/step-models", f.project_id),
        Some(json!({ "step": "group_review", "model": "  " })),
    )
    .await;
    let reply: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        reply["ok"],
        json!(true),
        "a blank clears, it is not refused: {text}"
    );
    let after = f.state.projects().get(&f.project_id).unwrap();
    assert_eq!(after.review.group_review_model, "");
    assert_eq!(
        after.review.escalation_floor, before.review.escalation_floor,
        "the other review settings are untouched"
    );
    let controls = camerata_server::cost_cap::RunControls {
        review: after.review.clone(),
        ..camerata_server::cost_cap::RunControls::default()
    };
    assert_eq!(
        controls.group_review_model_for(),
        camerata_api_types::project::STAGE2_DEFAULT_MODEL,
        "an unset group review model resolves to the built-in default"
    );
}

#[tokio::test]
async fn applying_a_model_profile_keeps_the_inspection_models_and_sets_the_development_fields() {
    let f = review_fixture(6);
    let id = f.project_id.clone();
    // The operator's inspection picks, set through the project store.
    f.state
        .projects()
        .update(&id, |p| {
            p.step_models.audit = "claude-opus-5-5".to_string();
            p.step_models.calibration = "claude-sonnet-5-5".to_string();
            p.step_models.fix = "claude-opus-5-5".to_string();
        })
        .unwrap();
    let before = f.state.projects().get(&id).unwrap();
    let (status, text) = call(
        &f.app,
        "POST",
        &format!("/api/projects/{id}/model-profile"),
        Some(json!({ "profile": "max_efficiency" })),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    let after = f.state.projects().get(&id).unwrap();
    assert_eq!(after.step_models.audit, before.step_models.audit);
    assert_eq!(
        after.step_models.calibration,
        before.step_models.calibration
    );
    assert_eq!(after.step_models.fix, before.step_models.fix);
    assert_ne!(
        after.step_models.research_chat, before.step_models.research_chat,
        "the development fields still follow the profile"
    );
}

#[tokio::test]
async fn the_step_models_route_sets_the_project_finder_and_it_reads_back_from_the_store() {
    let f = review_fixture(6);
    let before = f.state.projects().get(&f.project_id).unwrap();
    assert_eq!(
        before.review.finder,
        camerata_api_types::project::ProjectFinder::Agent,
        "a new project starts on the agent finder"
    );
    let (status, text) = call(
        &f.app,
        "POST",
        &format!("/api/projects/{}/step-models", f.project_id),
        Some(json!({ "step": "finder", "model": "engine" })),
    )
    .await;
    assert_eq!(status, 200, "{text}");
    let reply: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(reply["ok"], json!(true), "{text}");
    let after = f.state.projects().get(&f.project_id).unwrap();
    assert_eq!(
        after.review.finder,
        camerata_api_types::project::ProjectFinder::Engine
    );
    assert_eq!(
        after.review.group_review_model, before.review.group_review_model,
        "the other review settings are untouched"
    );
    // The settings view the screen reads carries it.
    let (_, text) = call(
        &f.app,
        "GET",
        &format!("/api/projects/{}/review/settings", f.project_id),
        None,
    )
    .await;
    let view: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(view["stage_models"]["finder"], json!("engine"), "{text}");
}

#[tokio::test]
async fn an_unknown_finder_word_is_refused_by_name_and_changes_nothing() {
    let f = review_fixture(6);
    let (_, text) = call(
        &f.app,
        "POST",
        &format!("/api/projects/{}/step-models", f.project_id),
        Some(json!({ "step": "finder", "model": "robot" })),
    )
    .await;
    let reply: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(reply["ok"], json!(false), "{text}");
    assert_eq!(
        reply["message"],
        json!("the finder must be agent or engine"),
        "{text}"
    );
    assert_eq!(
        f.state.projects().get(&f.project_id).unwrap().review.finder,
        camerata_api_types::project::ProjectFinder::Agent
    );
}

/// The finder model and the clone judge model have their own steps (the finder KIND keeps
/// `finder`): each sets, shows in the settings view, and clears with a blank model.
#[tokio::test]
async fn the_finder_model_and_clone_judge_model_steps_set_and_clear() {
    let f = review_fixture(6);
    let url = format!("/api/projects/{}/step-models", f.project_id);
    for (step, view_key) in [
        ("finder_model", "finder_model"),
        ("clone_judge", "clone_judge"),
        ("duplicate_check", "duplicate_check"),
    ] {
        let (_, text) = call(
            &f.app,
            "POST",
            &url,
            Some(json!({ "step": step, "model": " claude-opus-5-5 " })),
        )
        .await;
        let reply: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(reply["ok"], json!(true), "{step}: {text}");
        let review = f.state.projects().get(&f.project_id).unwrap().review;
        let stored = match step {
            "finder_model" => review.finder_model.clone(),
            "clone_judge" => review.clone_judge_model.clone(),
            _ => review.dup_check_model.clone(),
        };
        assert_eq!(stored, "claude-opus-5-5", "{step}");
        let (_, text) = call(
            &f.app,
            "GET",
            &format!("/api/projects/{}/review/settings", f.project_id),
            None,
        )
        .await;
        let view: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(
            view["stage_models"][view_key],
            json!("claude-opus-5-5"),
            "{step}: {text}"
        );
        // A blank model clears the setting back to the built-in default.
        let (_, text) = call(
            &f.app,
            "POST",
            &url,
            Some(json!({ "step": step, "model": "" })),
        )
        .await;
        let reply: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(reply["ok"], json!(true), "{step} cleared: {text}");
        let review = f.state.projects().get(&f.project_id).unwrap().review;
        let stored = match step {
            "finder_model" => review.finder_model,
            "clone_judge" => review.clone_judge_model,
            _ => review.dup_check_model,
        };
        assert!(stored.is_empty(), "{step} cleared");
    }
    // The finder KIND step still works, and an unknown step is still refused.
    let (_, text) = call(
        &f.app,
        "POST",
        &url,
        Some(json!({ "step": "finder", "model": "engine" })),
    )
    .await;
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap()["ok"],
        json!(true),
        "{text}"
    );
    let (_, text) = call(
        &f.app,
        "POST",
        &url,
        Some(json!({ "step": "no_such_step", "model": "claude-opus-5-5" })),
    )
    .await;
    let reply: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(reply["ok"], json!(false), "{text}");
    assert_eq!(reply["message"], json!("unknown step"), "{text}");
}
