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
async fn a_blank_group_review_model_is_refused_and_changes_nothing() {
    let f = review_fixture(6);
    let before = f.state.projects().get(&f.project_id).unwrap();
    let (_, text) = call(
        &f.app,
        "POST",
        &format!("/api/projects/{}/step-models", f.project_id),
        Some(json!({ "step": "group_review", "model": "  " })),
    )
    .await;
    let reply: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(reply["ok"], json!(false), "{text}");
    // The unknown-step branch also answers ok:false, so the reason is what pins this route.
    assert_eq!(
        reply["message"],
        json!("model must not be empty"),
        "the blank model is refused by name, not as an unknown step: {text}"
    );
    assert_eq!(
        f.state
            .projects()
            .get(&f.project_id)
            .unwrap()
            .review
            .group_review_model,
        before.review.group_review_model
    );
}

#[tokio::test]
async fn the_step_models_route_sets_the_project_finder_and_it_reads_back_from_the_store() {
    let f = review_fixture(6);
    let before = f.state.projects().get(&f.project_id).unwrap();
    assert_eq!(
        before.review.finder,
        camerata_api_types::project::ProjectFinder::Agent,
        "a project starts on the agent finder (the default)"
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
        Some(json!({ "step": "finder", "model": "agent" })),
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
