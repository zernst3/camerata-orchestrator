use super::*;
use crate::gallery::settings_states::{page_props, render};
use camerata_api_types::review::EscalationReason;
use camerata_ui::review_source::FixtureReviewSource;
use camerata_ui_core::review_settings::budget_for;
use camerata_ui_core::test_support::review_fixtures::{dataset_base, Body, Loaded};

/// The six states of the spec, in order.
const STATES: [&str; 6] = [
    "settings/defaults",
    "settings/rate-changed",
    "settings/invalid",
    "settings/saving",
    "settings/error",
    "settings/run-in-flight",
];

/// The markup of the lane alone (no stylesheet, no document), for assertions on the text.
fn fragment(id: &str) -> String {
    let props = page_props(id).expect("a settings state");
    crate::test_harness::render_fragment(crate::gallery::settings_states::SettingsStatePage, props)
}

/// The opening tag of the first element carrying `class`.
fn tag_with_class(html: &str, class: &str) -> String {
    let at = html
        .find(&format!("class=\"{class}"))
        .or_else(|| html.find(class))
        .unwrap_or_else(|| panic!("no element with {class}: {html}"));
    let start = html[..at].rfind('<').unwrap();
    let end = html[at..].find('>').unwrap() + at;
    html[start..=end].to_string()
}

fn view() -> SettingsData {
    dataset_base().settings
}

#[test]
fn defaults_show_the_saved_formula_and_a_disabled_apply() {
    let html = fragment("settings/defaults");
    let v = view();
    for text in [
        "Stage models",
        "Scan model",
        "Calibration (stage 1)",
        "Group review (stage 2)",
        "Fix model",
        "per site, batched",
        "one call per rule group, real time",
        "Escalation formula",
        "Rate per 1,000 lines",
        "Floor",
        "May exceed the budget",
        "unauthenticated reach",
        "secret exposure",
        "data loss",
        "cross tenant",
        "evidence unverified",
        APPLY_NOTE,
        "Engine Check",
        "Sample size per run",
        "0 turns the lane off; suggested 50 for early engagements",
    ] {
        assert!(html.contains(text), "{text}\n{html}");
    }
    let budget = budget_for(
        v.escalation.counted_lines,
        v.escalation.rate_per_kloc,
        v.escalation.floor,
    );
    assert!(
        html.contains(&format!(
            "Counted lines {} \u{2192} budget {budget}",
            camerata_ui_core::review_settings::group_thousands(v.escalation.counted_lines)
        )),
        "{html}"
    );
    assert!(
        tag_with_class(&html, "review-settings-apply").contains("disabled"),
        "nothing changed, nothing to apply"
    );
    assert!(!html.contains("review-settings-bad"), "no hint when valid");
    assert!(!html.contains("review-settings-banner"));
    assert!(!html.contains("review-settings-locked"));
    // The group review model is the fixture's, selected.
    assert!(
        html.contains("value=\"claude-fable-5-1\" selected"),
        "{html}"
    );
    // The fix model offers "(not set)".
    assert!(html.contains("(not set)"));
}

#[test]
fn a_changed_rate_shows_the_new_budget_and_what_applying_would_do() {
    let html = fragment("settings/rate-changed");
    let v = view();
    let doubled = budget_for(v.escalation.counted_lines, 2.0, v.escalation.floor);
    assert!(
        html.contains(&format!("\u{2192} budget {doubled}")),
        "the budget recomputes from the typed rate: {html}"
    );
    assert!(html.contains("value=\"2.0\""), "{html}");
    assert!(
        html.contains("would move above the line. Applying makes no model call."),
        "a preview says what applying WOULD do, not that it happened: {html}"
    );
    assert!(!html.contains("No model call was made"));
    assert!(
        !tag_with_class(&html, "review-settings-apply").contains("disabled"),
        "a valid change can be applied"
    );
    assert!(html.contains(">Apply<"));
}

#[test]
fn an_invalid_value_shows_its_hint_in_red_and_disables_apply() {
    let html = fragment("settings/invalid");
    assert!(
        html.contains("The rate must be between 0 and 10 escalations per 1,000 lines."),
        "{html}"
    );
    assert!(html.contains("review-settings-bad"));
    assert!(html.contains("aria-invalid=\"true\""));
    assert!(tag_with_class(&html, "review-settings-apply").contains("disabled"));
    let v = view();
    assert!(
        !html.contains(&format!("\u{2192} budget")),
        "no budget is claimed for a rate that cannot be applied: {html}"
    );
    assert!(html.contains(&format!(
        "Counted lines {}",
        camerata_ui_core::review_settings::group_thousands(v.escalation.counted_lines)
    )));
}

#[test]
fn saving_shows_the_word_and_locks_the_controls() {
    let html = fragment("settings/saving");
    assert!(html.contains(">Saving<"), "{html}");
    assert!(tag_with_class(&html, "review-settings-apply").contains("disabled"));
    assert!(tag_with_class(&html, "review-settings-num").contains("disabled"));
    assert!(tag_with_class(&html, "audit-model-select").contains("disabled"));
    assert!(!html.contains("review-settings-banner"));
}

#[test]
fn an_error_shows_the_servers_reason_in_a_banner_and_keeps_the_typed_values() {
    let html = fragment("settings/error");
    assert!(
        html.contains("The setting was not saved: the project file is read only."),
        "{html}"
    );
    assert!(tag_with_class(&html, "review-settings-banner").contains("role=\"alert\""));
    assert!(
        html.contains("value=\"2.0\""),
        "the fields keep the typed values: {html}"
    );
    assert!(
        html.contains(">Apply<"),
        "Apply is available again to retry: {html}"
    );
    assert!(!tag_with_class(&html, "review-settings-apply").contains("disabled"));
}

#[test]
fn a_run_in_flight_locks_every_control_and_says_why() {
    let html = fragment("settings/run-in-flight");
    assert!(html.contains(RUN_IN_FLIGHT_NOTE), "{html}");
    let selects = html.matches("<select").count();
    assert_eq!(selects, 4, "one per stage");
    for chunk in html.split("<select").skip(1) {
        let tag = &chunk[..chunk.find('>').unwrap()];
        assert!(tag.contains("disabled"), "a select is still live: {tag}");
    }
    for chunk in html.split("<input").skip(1) {
        let tag = &chunk[..chunk.find('>').unwrap()];
        assert!(tag.contains("disabled"), "an input is still live: {tag}");
    }
    assert!(tag_with_class(&html, "review-settings-apply").contains("disabled"));
}

#[test]
fn settings_lane_never_mentions_environment_models() {
    let allowed = "never the environment";
    let mut seen_the_sentence = 0;
    for id in STATES {
        let html = fragment(id);
        assert!(!html.is_empty());
        if html.contains(allowed) {
            seen_the_sentence += 1;
        }
        // Take out the one sentence the lane may use, then nothing may name the environment.
        let rest = html.replace(allowed, "").to_lowercase();
        for banned in ["env", ".env", "environment variable"] {
            assert!(!rest.contains(banned), "{id} mentions {banned:?}: {html}");
        }
    }
    assert_eq!(
        seen_the_sentence,
        STATES.len(),
        "every state carries the sentence that models come from the project or a CLI flag"
    );
    // The whole page too (heading and card), not only the lane.
    for id in STATES {
        let page = render(id).replace(crate::style::GLOBAL_CSS, "");
        let rest = page.replace(allowed, "").to_lowercase();
        assert!(!rest.contains("environment"), "{id}");
    }
}

#[test]
fn no_settings_state_has_a_dash_in_its_text() {
    for id in STATES {
        let page = render(id).replace(crate::style::GLOBAL_CSS, "");
        assert!(!page.contains('\u{2014}'), "{id}");
    }
}

#[test]
fn every_settings_state_builds_from_a_ready_fixture() {
    for id in STATES {
        let props = page_props(id).expect(id);
        assert!(matches!(props.body.view, Loaded::Ready(_)), "{id}");
    }
    assert!(page_props("tree/markers").is_none());
    let _ = Body::ShellOnly;
}

#[test]
fn the_host_draws_loading_until_the_source_answers() {
    #[derive(Clone, PartialEq, Props)]
    struct Case {
        source: SettingsSource,
    }
    fn harness(case: Case) -> Element {
        rsx! {
            ReviewSettingsHost { source: case.source.clone(), on_applied: move |_| {} }
        }
    }
    let html = crate::test_harness::render_fragment(
        harness,
        Case {
            source: SettingsSource(Rc::new(FixtureReviewSource::base())),
        },
    );
    assert!(html.contains("Loading settings"), "{html}");
}

// ── Apply: one request, no model ───────────────────────────────────────────────────────────

#[tokio::test]
async fn changing_the_rate_after_a_run_re_marks_without_a_model_call() {
    let source = FixtureReviewSource::base();
    let v = load(&source).await.unwrap();
    let before_budget = v.escalation.budget.budget;
    let mut draft = SettingsDraft::from_view(&v);
    draft.rate = "2.0".to_string();
    let first_call = source.calls().len();

    let (outcome, reloaded) = apply_formula(&source, &v, &draft, false).await.unwrap();

    let calls: Vec<String> = source.calls().split_off(first_call);
    assert_eq!(
        calls,
        ["apply_budget", "settings"],
        "exactly one escalation-budget request, then the reload, and nothing else"
    );
    assert!(outcome.applied);
    assert_eq!(outcome.after.rate_per_kloc, 2.0);
    assert!(
        effect_sentence(&outcome).ends_with("No model call was made."),
        "{}",
        effect_sentence(&outcome)
    );
    assert_eq!(
        reloaded.escalation.rate_per_kloc, 2.0,
        "the reload shows it"
    );
    assert!(reloaded.escalation.budget.budget >= before_budget);
    assert_eq!(
        reloaded.escalation.budget.budget,
        budget_for(v.escalation.counted_lines, 2.0, v.escalation.floor),
        "the server's line is the preview's line"
    );
    // Lowering it again is the same one request.
    let mut draft = SettingsDraft::from_view(&reloaded);
    draft.rate = "0.5".to_string();
    let n = source.calls().len();
    apply_formula(&source, &reloaded, &draft, false)
        .await
        .unwrap();
    assert_eq!(
        source.calls().split_off(n),
        ["apply_budget", "settings"],
        "raising or lowering, never more than the one write"
    );
}

#[tokio::test]
async fn apply_sends_nothing_when_the_draft_is_invalid_unchanged_or_a_run_is_in_flight() {
    let source = FixtureReviewSource::base();
    let v = load(&source).await.unwrap();
    let n = source.calls().len();

    let mut bad = SettingsDraft::from_view(&v);
    bad.rate = "11".into();
    let err = apply_formula(&source, &v, &bad, false).await.unwrap_err();
    assert!(err.contains("between 0 and 10"), "{err}");

    let same = SettingsDraft::from_view(&v);
    assert!(apply_formula(&source, &v, &same, false).await.is_err());

    let mut changed = SettingsDraft::from_view(&v);
    changed.rate = "3.0".into();
    let err = apply_formula(&source, &v, &changed, true)
        .await
        .unwrap_err();
    assert_eq!(err, RUN_IN_FLIGHT_NOTE);

    assert_eq!(source.calls().len(), n, "no request left the screen");
}

#[tokio::test]
async fn a_refused_write_keeps_the_error_and_leaves_the_saved_settings() {
    let source = FixtureReviewSource::failing("the project file is read only");
    let v = dataset_base().settings;
    let mut draft = SettingsDraft::from_view(&v);
    draft.floor = "35".into();
    let err = apply_formula(&source, &v, &draft, false).await.unwrap_err();
    assert_eq!(err, "the project file is read only");
    assert_eq!(
        source.calls(),
        ["apply_budget"],
        "no reload after a refusal"
    );
}

#[tokio::test]
async fn the_sample_size_saves_with_one_write_and_a_reload() {
    let source = FixtureReviewSource::base();
    let v = load(&source).await.unwrap();
    let mut draft = SettingsDraft::from_view(&v);
    draft.sample = "0".into();
    let n = source.calls().len();
    let reloaded = save_sample(&source, &v, &draft, false).await.unwrap();
    assert_eq!(reloaded.engine_check.sample_size_per_run, 0);
    assert_eq!(source.calls().split_off(n), ["set_sample_size", "settings"]);
    // Over the limit: refused before any request.
    draft.sample = "1001".into();
    let n = source.calls().len();
    assert!(save_sample(&source, &reloaded, &draft, false)
        .await
        .is_err());
    assert_eq!(source.calls().len(), n);
}

#[tokio::test]
async fn a_stage_model_goes_through_the_step_models_route_under_its_step_key() {
    let source = FixtureReviewSource::base();
    let n = source.calls().len();
    let reloaded = change_stage_model(&source, Stage::GroupReview, "claude-opus-5-5", false)
        .await
        .unwrap();
    assert_eq!(reloaded.stage_models.group_review, "claude-opus-5-5");
    assert_eq!(
        source.calls().split_off(n),
        ["set_stage_model:group_review", "settings"]
    );
    let cleared = change_stage_model(&source, Stage::Fix, "", false)
        .await
        .unwrap();
    assert_eq!(cleared.stage_models.fix, "", "the fix model can be unset");
    assert!(change_stage_model(&source, Stage::Scan, "x", true)
        .await
        .is_err());
    assert!(
        change_stage_model(&source, Stage::GroupReview, " ", false)
            .await
            .is_err(),
        "a blank stage 2 model is refused"
    );
}

#[tokio::test]
async fn previewing_the_line_writes_nothing() {
    let source = FixtureReviewSource::base();
    let v = load(&source).await.unwrap();
    let mut draft = SettingsDraft::from_view(&v);
    draft.rate = "5".into();
    draft.reasons = vec![EscalationReason::DataLoss];
    let req = preview_request(&v, &draft).expect("a changed valid formula");
    let outcome = source.preview_budget(req).await.unwrap();
    assert!(!outcome.applied);
    let after = load(&source).await.unwrap();
    assert_eq!(after, v, "the saved settings are unchanged by a preview");
}
