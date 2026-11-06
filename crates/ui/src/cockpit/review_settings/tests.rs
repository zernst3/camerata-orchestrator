use super::*;
use crate::gallery::settings_states::{page_props, render};
use camerata_api_types::review::EscalationReason;
use camerata_ui::review_io::ReviewWrites;
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
        !html.contains("\u{2192} budget"),
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
        api: ReviewApi,
    }
    fn harness(case: Case) -> Element {
        rsx! {
            ReviewSettingsHost { api: case.api.clone(), on_applied: move |_| {} }
        }
    }
    let fixture = std::rc::Rc::new(FixtureReviewSource::base());
    let html = crate::test_harness::render_fragment(
        harness,
        Case {
            api: ReviewApi {
                reads: fixture.clone(),
                writes: fixture,
            },
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

    let (outcome, reloaded) = apply_formula(&source, &source, &v, &draft, false)
        .await
        .unwrap();

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
    apply_formula(&source, &source, &reloaded, &draft, false)
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
    let err = apply_formula(&source, &source, &v, &bad, false)
        .await
        .unwrap_err();
    assert!(err.contains("between 0 and 10"), "{err}");

    let same = SettingsDraft::from_view(&v);
    assert!(apply_formula(&source, &source, &v, &same, false)
        .await
        .is_err());

    let mut changed = SettingsDraft::from_view(&v);
    changed.rate = "3.0".into();
    let err = apply_formula(&source, &source, &v, &changed, true)
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
    let err = apply_formula(&source, &source, &v, &draft, false)
        .await
        .unwrap_err();
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
    let reloaded = save_sample(&source, &source, &v, &draft, false)
        .await
        .unwrap();
    assert_eq!(reloaded.engine_check.sample_size_per_run, 0);
    assert_eq!(source.calls().split_off(n), ["set_sample_size", "settings"]);
    // Over the limit: refused before any request.
    draft.sample = "1001".into();
    let n = source.calls().len();
    assert!(save_sample(&source, &source, &reloaded, &draft, false)
        .await
        .is_err());
    assert_eq!(source.calls().len(), n);
}

#[tokio::test]
async fn a_stage_model_goes_through_the_step_models_route_under_its_step_key() {
    let source = FixtureReviewSource::base();
    let n = source.calls().len();
    let reloaded = change_stage_model(
        &source,
        &source,
        Stage::GroupReview,
        "claude-opus-5-5",
        false,
    )
    .await
    .unwrap();
    assert_eq!(reloaded.stage_models.group_review, "claude-opus-5-5");
    assert_eq!(
        source.calls().split_off(n),
        ["set_stage_model:group_review", "settings"]
    );
    let cleared = change_stage_model(&source, &source, Stage::Fix, "", false)
        .await
        .unwrap();
    assert_eq!(cleared.stage_models.fix, "", "the fix model can be unset");
    assert!(change_stage_model(&source, &source, Stage::Scan, "x", true)
        .await
        .is_err());
    assert!(
        change_stage_model(&source, &source, Stage::GroupReview, " ", false)
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

/// The opening tag of the checkbox for `label` (the `<input` just before its words).
fn checkbox_for(html: &str, label: &str) -> String {
    let at = html
        .find(&format!("<span>{label}</span>"))
        .unwrap_or_else(|| panic!("no checkbox labelled {label}: {html}"));
    let start = html[..at].rfind("<input").unwrap();
    let end = html[start..].find('>').unwrap() + start;
    html[start..=end].to_string()
}

#[test]
fn evidence_unverified_is_drawn_ticked_and_disabled_in_every_state() {
    for id in STATES {
        let html = fragment(id);
        let tag = checkbox_for(&html, "evidence unverified");
        assert!(tag.contains("checked"), "{id}: ticked: {tag}");
        assert!(tag.contains("disabled"), "{id}: locked: {tag}");
        // The others stay ordinary checkboxes (not locked) in a state with nothing in flight.
        if id == "settings/defaults" {
            let tag = checkbox_for(&html, "secret exposure");
            assert!(!tag.contains("disabled"), "{tag}");
        }
    }
}

#[test]
fn an_invalid_rate_says_the_budget_was_not_computed() {
    let html = fragment("settings/invalid");
    assert!(
        html.contains("Counted lines 12,000, budget not computed"),
        "the line says what is missing rather than going silent: {html}"
    );
}

/// The body of the first CSS rule whose selector is exactly `selector`.
fn css_rule(selector: &str) -> String {
    let css = crate::style::GLOBAL_CSS;
    let at = css
        .find(&format!("{selector} {{"))
        .unwrap_or_else(|| panic!("no CSS rule for {selector}"));
    let open = css[at..].find('{').unwrap() + at;
    let close = css[open..].find('}').unwrap() + open;
    css[open + 1..close].to_string()
}

#[test]
fn a_disabled_apply_has_the_inert_look_not_the_amber_bevel() {
    // The global theme gives .btn-run a 2px amber bevel, a drop shadow and a text shadow that
    // outlive its :disabled colours; the lane's own rule (higher specificity) takes them off.
    let rule = css_rule(".review-settings-apply.review-settings-apply:disabled");
    for want in [
        "border-color: var(--line)",
        "box-shadow: none",
        "text-shadow: none",
    ] {
        assert!(rule.contains(want), "{want} in {rule}");
    }
    // Every state with a disabled Apply carries the class that rule is on.
    for id in [
        "settings/defaults",
        "settings/invalid",
        "settings/saving",
        "settings/run-in-flight",
    ] {
        let tag = tag_with_class(&fragment(id), "review-settings-apply");
        assert!(tag.contains("disabled"), "{id}: {tag}");
    }
}

// ── The host's stale-preview guard (`still_asked`) ────────────────────────────────────────

use camerata_api_types::review_view::{EscalationBudgetReq, SampleSizeResp};
use camerata_ui::review_source::{ReviewSourceError, SourceFuture};
use std::cell::RefCell;
use std::rc::Rc;

/// A source whose preview answer is held back until the test lets it go, so a reply can arrive
/// after the draft moved on. Every other call goes to the fixture.
struct HeldPreview {
    inner: FixtureReviewSource,
    gate: RefCell<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl ReviewWrites for HeldPreview {
    fn post_decision(
        &self,
        req: camerata_api_types::decisions::NewDecisionReq,
    ) -> SourceFuture<'_, camerata_api_types::decisions::DecisionRecorded> {
        self.inner.post_decision(req)
    }
    fn revoke_decision<'a>(
        &'a self,
        id: &'a str,
    ) -> SourceFuture<'a, camerata_api_types::decisions::RevokeResp> {
        self.inner.revoke_decision(id)
    }
    fn post_edit(
        &self,
        req: camerata_api_types::edits::NewEditReq,
    ) -> SourceFuture<'_, camerata_api_types::edits::EditRecorded> {
        self.inner.post_edit(req)
    }
    fn preview_budget(
        &self,
        req: EscalationBudgetReq,
    ) -> SourceFuture<'_, EscalationBudgetOutcome> {
        let gate = self.gate.borrow_mut().take();
        Box::pin(async move {
            if let Some(gate) = gate {
                gate.await.map_err(|_| ReviewSourceError {
                    message: "the gate was dropped".to_string(),
                })?;
            }
            self.inner.preview_budget(req).await
        })
    }
    fn apply_budget(&self, req: EscalationBudgetReq) -> SourceFuture<'_, EscalationBudgetOutcome> {
        self.inner.apply_budget(req)
    }
    fn set_sample_size(&self, n: usize) -> SourceFuture<'_, SampleSizeResp> {
        self.inner.set_sample_size(n)
    }
    fn set_stage_model<'a>(&'a self, step: &'a str, model: &'a str) -> SourceFuture<'a, ()> {
        self.inner.set_stage_model(step, model)
    }
    fn post_engine_verdict<'a>(
        &'a self,
        key: &'a str,
        req: camerata_api_types::review_view::EngineCheckVerdictReq,
    ) -> SourceFuture<'a, camerata_api_types::review_view::EngineCheckVerdict> {
        self.inner.post_engine_verdict(key, req)
    }
    fn set_needed_me<'a>(
        &'a self,
        decision_id: &'a str,
        needed_me: bool,
    ) -> SourceFuture<'a, camerata_api_types::decisions::Decision> {
        self.inner.set_needed_me(decision_id, needed_me)
    }
}

type PreviewSignals = (
    Signal<Option<SettingsData>>,
    Signal<Option<SettingsDraft>>,
    Signal<Option<EscalationBudgetOutcome>>,
);

thread_local! {
    static PREVIEW_SIGNALS: RefCell<Option<PreviewSignals>> = const { RefCell::new(None) };
    static PREVIEW_API: RefCell<Option<ReviewApi>> = const { RefCell::new(None) };
}

/// A component that asks for the preview of a typed rate the way the host does: it asks, and the
/// draft signal is set to the same draft.
fn preview_app() -> Element {
    let view = use_signal(|| Some(dataset_base().settings));
    let typed = {
        let v = dataset_base().settings;
        let mut d = SettingsDraft::from_view(&v);
        d.rate = "2.0".to_string();
        d
    };
    let draft = use_signal(|| Some(typed.clone()));
    let preview = use_signal(|| None::<EscalationBudgetOutcome>);
    PREVIEW_SIGNALS.with(|s| *s.borrow_mut() = Some((view, draft, preview)));
    use_hook(|| {
        let api = PREVIEW_API.with(|a| a.borrow().clone()).expect("an api");
        request_preview(&api, &dataset_base().settings, &typed, view, draft, preview);
    });
    rsx! {}
}

/// Ask for a preview, hold the reply, run `meanwhile` (which may change the draft), let the reply
/// arrive, and return what the host kept.
async fn preview_after(
    meanwhile: impl FnOnce(&VirtualDom, &PreviewSignals),
) -> Option<EscalationBudgetOutcome> {
    let (release, gate) = tokio::sync::oneshot::channel();
    let held = Rc::new(HeldPreview {
        inner: FixtureReviewSource::base(),
        gate: RefCell::new(Some(gate)),
    });
    PREVIEW_API.with(|a| {
        *a.borrow_mut() = Some(ReviewApi {
            reads: Rc::new(FixtureReviewSource::base()),
            writes: held,
        });
    });
    let mut vdom = VirtualDom::new(preview_app);
    vdom.rebuild_in_place();
    // Let the spawned preview request start and wait on its gate.
    let _ = tokio::time::timeout(std::time::Duration::from_millis(50), vdom.wait_for_work()).await;
    let signals = PREVIEW_SIGNALS
        .with(|s| *s.borrow())
        .expect("the component ran");
    meanwhile(&vdom, &signals);
    release.send(()).expect("the request is waiting");
    for _ in 0..4 {
        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(50), vdom.wait_for_work()).await;
        vdom.render_immediate(&mut dioxus::dioxus_core::NoOpMutations);
    }
    vdom.in_runtime(|| signals.2.peek().clone())
}

#[tokio::test]
async fn a_preview_reply_is_kept_while_the_draft_still_asks_for_it() {
    let kept = preview_after(|_, _| {}).await;
    let outcome = kept.expect("the answer to the question still being asked is shown");
    assert!(!outcome.applied);
    assert_eq!(outcome.after.rate_per_kloc, 2.0);
}

#[tokio::test]
async fn a_preview_reply_is_dropped_when_the_draft_moved_on_before_it_came() {
    let kept = preview_after(|vdom, (_, draft, _)| {
        // The reviewer typed a different rate while the request was out.
        vdom.in_runtime(|| {
            let mut next = draft.peek().clone().expect("a draft");
            next.rate = "3.0".to_string();
            let mut draft = *draft;
            draft.set(Some(next));
        });
    })
    .await;
    assert_eq!(kept, None, "a slow reply never shows over a newer draft");
}

#[tokio::test]
async fn a_preview_reply_is_dropped_when_the_draft_is_back_to_what_is_saved() {
    let kept = preview_after(|vdom, (view, draft, _)| {
        vdom.in_runtime(|| {
            let saved = view.peek().clone().expect("a view");
            let mut draft = *draft;
            draft.set(Some(SettingsDraft::from_view(&saved)));
        });
    })
    .await;
    assert_eq!(kept, None, "reverting the field cancels the pending answer");
}
