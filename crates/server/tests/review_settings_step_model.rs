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
