//! Tests of the Inspection settings component (settings overhaul, work order A).
//!
//! Three layers: the real props-only screen rendered from the gallery's states (the words the
//! operator sees), the real host over a fake [`InspectionIo`] (one write per change, a refusal
//! keeps the old value, a run in flight locks it), and the real inspection start screen over a
//! stub server (the acceptance test: the Opus recipe set before any run).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use dioxus::dioxus_core::{AttributeValue, ElementId, Mutation, Mutations};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;
use crate::gallery::inspection_settings_states::{page_props, render};

// ── The states, as the operator reads them ──────────────────────────────────────────────

/// The markup of a state page without the stylesheet.
fn page(id: &str) -> String {
    render(&format!("inspection-settings/{id}")).replace(crate::style::GLOBAL_CSS, "")
}

/// The opening tag of the element with html id `id`.
fn tag_of(html: &str, id: &str) -> String {
    let at = html
        .find(&format!("id=\"{id}\""))
        .unwrap_or_else(|| panic!("no element {id}: {html}"));
    let start = html[..at].rfind('<').unwrap();
    let end = html[at..].find('>').unwrap() + at;
    html[start..=end].to_string()
}

/// The select with html id `id`, tag and options.
fn select_of(html: &str, id: &str) -> String {
    let at = html
        .find(&format!("id=\"{id}\""))
        .unwrap_or_else(|| panic!("no select {id}: {html}"));
    let end = html[at..].find("</select>").unwrap() + at;
    html[at..end].to_string()
}

/// The order the labelled rows appear in.
fn row_order(html: &str) -> Vec<&'static str> {
    let mut found: Vec<(usize, &'static str)> = camerata_ui_core::inspection_settings::Stage::ALL
        .iter()
        .filter_map(|s| {
            html.find(&format!("id=\"is-model-{}\"", s.step_key()))
                .map(|at| (at, s.label()))
        })
        .collect();
    found.sort();
    found.into_iter().map(|(_, l)| l).collect()
}

#[test]
fn the_opus_recipe_shows_in_the_designed_row_order_with_its_values_selected() {
    let html = page("agent-opus");
    assert_eq!(
        row_order(&html),
        [
            "Finder",
            "Finder model",
            "Group review model",
            "Fix model",
            "Duplicate check model",
            "Clone judge model"
        ]
    );
    assert!(select_of(&html, "is-model-finder").contains("value=\"agent\" selected"));
    assert!(
        select_of(&html, "is-model-finder_model").contains("value=\"claude-opus-5-5\" selected")
    );
    assert!(
        select_of(&html, "is-model-group_review").contains("value=\"claude-opus-5-5\" selected")
    );
    assert!(select_of(&html, "is-model-fix").contains("value=\"claude-haiku-5-5\" selected"));
    assert!(select_of(&html, "is-model-duplicate_check")
        .contains("value=\"claude-sonnet-5-5\" selected"));
    // Scan model and Calibration model are not used by the Agent finder: hidden, with the
    // button that counts them.
    assert!(
        !html.contains("id=\"is-model-audit\"") && !html.contains("id=\"is-model-calibration\"")
    );
    assert!(
        html.contains("Show the 2 rows the Engine finder uses"),
        "{html}"
    );
    // Where it runs and who it is for, in the one component.
    for text in [
        "Where it runs",
        "Project backend",
        "Client engagement",
        "Client name",
        "Required before export",
    ] {
        assert!(html.contains(text), "{text}\n{html}");
    }
    assert!(html.contains("Saved"), "the row that just saved says so");
}

#[test]
fn the_engine_finder_shows_scan_and_calibration_and_not_the_finder_model() {
    let html = page("engine");
    assert_eq!(
        row_order(&html),
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
    assert!(
        html.contains("Show the 1 row the Agent finder uses"),
        "{html}"
    );
    assert!(
        !select_of(&html, "is-model-audit").contains("(not set"),
        "scan is always stored"
    );
    assert!(html.contains("Built-in default: Claude Sonnet 5.5."));
    assert!(html.contains("Built-in default: Claude Opus 5.5."));
}

#[test]
fn every_unset_row_names_its_built_in_default() {
    let html = page("all-unset");
    for (id, shown) in [
        ("is-model-finder_model", "(not set: Claude Opus 5.5)"),
        ("is-model-group_review", "(not set: Claude Opus 5.5)"),
        ("is-model-fix", "(not set: Claude Haiku 5.5)"),
        ("is-model-duplicate_check", "(not set: Claude Sonnet 5.5)"),
        ("is-model-clone_judge", "(not set: Claude Haiku 5.5)"),
    ] {
        let select = select_of(&html, id);
        assert!(select.contains(shown), "{id}: {select}");
        assert!(
            select.contains("value=\"\" selected"),
            "{id} shows not set: {select}"
        );
    }
}

#[test]
fn the_words_are_the_new_ones_and_the_old_labels_are_gone() {
    let html = page("agent-opus");
    for gone in [
        "Calibration (stage 1)",
        "Group review (stage 2)",
        "or a CLI flag",
        "Duplicate check<",
        "Clone judge<",
    ] {
        assert!(!html.contains(gone), "{gone} is gone: {html}");
    }
    assert!(html.contains("Models come from this project, never the environment."));
    assert!(html.contains("Decides which model rows below are used."));
}

#[test]
fn the_hidden_rows_show_marked_as_not_used_after_the_reveal() {
    let html = page("hidden-rows-shown");
    assert_eq!(row_order(&html).len(), 8, "{html}");
    assert!(html.contains("not used by the Agent finder"), "{html}");
    assert!(
        !html.contains("Show the 2 rows"),
        "the button is gone once open"
    );
}

#[test]
fn the_conflict_line_shows_for_cli_with_client_engagement_and_only_then() {
    let html = page("cli-client-conflict");
    assert!(html.contains("inspection-settings-conflict"), "{html}");
    assert!(html.contains("A client engagement runs on the API only."));
    for id in ["agent-opus", "engine", "all-unset", "run-in-flight"] {
        assert!(
            !page(id).contains("inspection-settings-conflict"),
            "{id} is not CLI with client engagement"
        );
    }
}

#[test]
fn a_run_in_flight_locks_every_select_and_says_why() {
    let html = page("run-in-flight");
    assert!(html.contains(RUN_IN_FLIGHT_NOTE), "{html}");
    let selects: Vec<&str> = html.split("<select").skip(1).collect();
    assert!(selects.len() >= 6, "{html}");
    for chunk in selects {
        let tag = &chunk[..chunk.find('>').unwrap()];
        assert!(tag.contains("disabled"), "a select is still live: {tag}");
    }
    let reveal = tag_of(&html, "is-reveal-unused");
    assert!(!reveal.contains("disabled"), "showing rows writes nothing");
}

#[test]
fn while_a_write_is_in_flight_every_select_is_locked() {
    #[derive(Clone, PartialEq, Props)]
    struct Case {
        project: ProjectView,
    }
    #[component]
    fn Saving(props: Case) -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        rsx! {
            InspectionSettingsView {
                project: props.project,
                saving: true,
                on_event: move |_| {},
            }
        }
    }
    let props = page_props("inspection-settings/agent-opus").expect("a state");
    let html = crate::test_harness::render_fragment(
        Saving,
        Case {
            project: props.project,
        },
    );
    let selects: Vec<&str> = html.split("<select").skip(1).collect();
    assert!(selects.len() >= 6, "{html}");
    for chunk in selects {
        let tag = &chunk[..chunk.find('>').unwrap()];
        assert!(
            tag.contains("disabled"),
            "a select is still live while saving: {tag}"
        );
    }
}

#[test]
fn a_refusal_shows_the_servers_words_in_an_alert() {
    let html = page("save-error");
    assert!(html.contains("model must not be empty"), "{html}");
    assert!(html.contains("role=\"alert\""));
}

#[test]
fn with_the_registry_missing_each_select_offers_its_current_value() {
    let html = page("registry-missing");
    assert!(
        select_of(&html, "is-model-finder_model").contains("value=\"claude-opus-5-5\" selected")
    );
    assert!(select_of(&html, "is-model-fix").contains("value=\"claude-haiku-5-5\" selected"));
    assert!(
        select_of(&html, "is-model-group_review").contains("value=\"claude-opus-5-5\" selected")
    );
}

#[test]
fn no_state_has_a_dash_or_names_the_environment_outside_its_one_sentence() {
    for id in [
        "agent-opus",
        "engine",
        "all-unset",
        "hidden-rows-shown",
        "cli-client-conflict",
        "run-in-flight",
        "save-error",
        "registry-missing",
    ] {
        let html = page(id);
        assert!(
            !html.contains('\u{2014}') && !html.contains('\u{2013}'),
            "{id}"
        );
        let rest = html.replace("never the environment", "").to_lowercase();
        assert!(!rest.contains("environment"), "{id}");
        assert!(
            html.contains("never the environment"),
            "{id} carries the sentence"
        );
    }
    assert!(page_props("inspection-settings/nope").is_none());
}

// ── The host over a fake ────────────────────────────────────────────────────────────────

/// Apply one step-models write to a project's JSON, as the server's route does.
fn apply_step(project: &mut Value, step: &str, model: &str) {
    let (section, key) = match step {
        "finder" => ("review", "finder"),
        "finder_model" => ("review", "finder_model"),
        "group_review" => ("review", "group_review_model"),
        "duplicate_check" => ("review", "dup_check_model"),
        "clone_judge" => ("review", "clone_judge_model"),
        "audit" => ("step_models", "audit"),
        "calibration" => ("step_models", "calibration"),
        "fix" => ("step_models", "fix"),
        other => panic!("unknown step {other}"),
    };
    project[section][key] = json!(model);
}

/// A new project as the server stores it.
fn new_project_json() -> Value {
    json!({
        "id": "p-1", "name": "Hearth", "repos": ["acme/hearth"],
        "review": serde_json::to_value(
            camerata_api_types::project::ReviewSettings::for_new_project()
        ).unwrap(),
        "step_models": serde_json::to_value(camerata_api_types::project::StepModels::default()).unwrap(),
        "backend": "api", "client_engagement": true, "client_name": "",
    })
}

fn registry() -> ModelsResp {
    use camerata_ui_core::models::ModelOption;
    let model = |id: &str, label: &str| ModelOption {
        label: label.to_string(),
        id: id.to_string(),
        provider: "claude".to_string(),
    };
    ModelsResp {
        models: vec![
            model("claude-opus-5-5", "Claude Opus 5.5"),
            model("claude-sonnet-5-5", "Claude Sonnet 5.5"),
            model("claude-haiku-5-5", "Claude Haiku 5.5"),
        ],
        default: String::new(),
        backend: String::new(),
    }
}

/// The fake: the project lives in it, every write is recorded, and one can be made to fail.
struct Fake {
    project: RefCell<Value>,
    writes: RefCell<Vec<(String, String, String)>>,
    refuse: Cell<Option<&'static str>>,
}

impl Fake {
    fn new() -> Rc<Self> {
        Rc::new(Self {
            project: RefCell::new(new_project_json()),
            writes: RefCell::new(Vec::new()),
            refuse: Cell::new(None),
        })
    }
}

impl InspectionIo for Fake {
    fn project<'a>(&'a self, _id: &'a str) -> IoFuture<'a, Option<ProjectView>> {
        Box::pin(async move { serde_json::from_value(self.project.borrow().clone()).ok() })
    }

    fn set_step<'a>(
        &'a self,
        id: &'a str,
        step: &'a str,
        model: &'a str,
    ) -> IoFuture<'a, Result<ProjectView, String>> {
        Box::pin(async move {
            self.writes
                .borrow_mut()
                .push((id.to_string(), step.to_string(), model.to_string()));
            if let Some(why) = self.refuse.get() {
                return Err(why.to_string());
            }
            apply_step(&mut self.project.borrow_mut(), step, model);
            serde_json::from_value(self.project.borrow().clone()).map_err(|e| e.to_string())
        })
    }

    fn models(&self) -> IoFuture<'_, Option<ModelsResp>> {
        Box::pin(async { Some(registry()) })
    }
}

/// A mounted screen: its virtual DOM and what it has told us about its elements.
struct Screen {
    vdom: VirtualDom,
    /// html `id` attribute to the element carrying it (the most recent).
    by_html_id: HashMap<String, ElementId>,
    /// Elements with a `change` or `click` listener.
    listeners: Vec<(String, ElementId)>,
}

impl Screen {
    fn mount(app: fn() -> Element) -> Self {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let mut vdom = VirtualDom::new(app);
        let mut screen = Self {
            by_html_id: HashMap::new(),
            listeners: Vec::new(),
            vdom: VirtualDom::new(|| rsx! {}),
        };
        screen.take(&vdom.rebuild_to_vec());
        screen.vdom = vdom;
        screen
    }

    fn take(&mut self, m: &Mutations) {
        for edit in &m.edits {
            match edit {
                Mutation::SetAttribute {
                    name: "id",
                    value: AttributeValue::Text(text),
                    id,
                    ..
                } => {
                    self.by_html_id.insert(text.clone(), *id);
                }
                Mutation::NewEventListener { name, id } => {
                    self.listeners.push((name.clone(), *id));
                }
                _ => {}
            }
        }
    }

    async fn pump(&mut self, ms: u64) {
        let until = tokio::time::Instant::now() + Duration::from_millis(ms);
        while tokio::time::Instant::now() < until {
            let _ =
                tokio::time::timeout(Duration::from_millis(10), self.vdom.wait_for_work()).await;
            let edits = self.vdom.render_immediate_to_vec();
            self.take(&edits);
        }
    }

    fn html(&self) -> String {
        dioxus_ssr::render(&self.vdom)
    }

    /// The operator picks `value` in the select with html id `html_id`.
    fn pick(&self, html_id: &str, value: &str) {
        let id = *self
            .by_html_id
            .get(html_id)
            .unwrap_or_else(|| panic!("no element {html_id}: {:?}", self.by_html_id.keys()));
        assert!(
            self.listeners
                .iter()
                .any(|(n, i)| n == "change" && *i == id),
            "{html_id} has no change listener"
        );
        let data: Rc<dyn std::any::Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedFormData::new(value.to_string(), Vec::new()),
        )));
        self.vdom
            .runtime()
            .handle_event("change", dioxus::dioxus_core::Event::new(data, true), id);
    }

    /// The operator sets the checkbox with html id `html_id` to `checked`.
    fn set_checked(&self, html_id: &str, checked: bool) {
        self.pick(html_id, if checked { "true" } else { "false" });
    }

    /// The operator clicks the element with html id `html_id`.
    fn click(&self, html_id: &str) {
        let id = *self
            .by_html_id
            .get(html_id)
            .unwrap_or_else(|| panic!("no element {html_id}"));
        let data: Rc<dyn std::any::Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedMouseData::default(),
        )));
        self.vdom
            .runtime()
            .handle_event("click", dioxus::dioxus_core::Event::new(data, true), id);
    }
}

thread_local! {
    static FAKE: RefCell<Option<(Rc<Fake>, bool)>> = const { RefCell::new(None) };
}

fn host_app() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    let (fake, run_in_flight) = FAKE.with(|f| f.borrow().clone().expect("the test set the fake"));
    rsx! {
        InspectionSettingsHost {
            project_id: "p-1".to_string(),
            run_in_flight,
            api: InspectionApi(fake),
        }
    }
}

async fn mounted(fake: &Rc<Fake>, run_in_flight: bool) -> Screen {
    FAKE.with(|f| *f.borrow_mut() = Some((fake.clone(), run_in_flight)));
    let mut screen = Screen::mount(host_app);
    screen.pump(300).await;
    screen
}

#[tokio::test(flavor = "current_thread")]
async fn a_new_project_shows_the_recipe_with_no_run_and_no_inspection_result() {
    let fake = Fake::new();
    let screen = mounted(&fake, false).await;
    let html = screen.html();
    assert_eq!(
        row_order(&html),
        [
            "Finder",
            "Finder model",
            "Group review model",
            "Fix model",
            "Duplicate check model",
            "Clone judge model"
        ],
        "{html}"
    );
    assert!(select_of(&html, "is-model-finder").contains("value=\"agent\" selected"));
    assert!(
        select_of(&html, "is-model-group_review").contains("value=\"claude-opus-5-5\" selected")
    );
    assert!(select_of(&html, "is-model-finder_model").contains("(not set: Claude Opus 5.5)"));
    assert!(
        fake.writes.borrow().is_empty(),
        "showing the settings writes nothing"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn changing_the_finder_sends_one_write_and_redraws_the_rows_from_the_returned_project() {
    let fake = Fake::new();
    let mut screen = mounted(&fake, false).await;
    screen.pick("is-model-finder", "engine");
    screen.pump(300).await;
    assert_eq!(
        *fake.writes.borrow(),
        [(
            "p-1".to_string(),
            "finder".to_string(),
            "engine".to_string()
        )],
        "exactly one write, under the finder step"
    );
    let html = screen.html();
    assert_eq!(
        row_order(&html),
        [
            "Finder",
            "Scan model",
            "Calibration model",
            "Group review model",
            "Fix model",
            "Duplicate check model",
            "Clone judge model"
        ],
        "{html}"
    );
    assert!(html.contains("Saved"), "{html}");
    assert!(select_of(&html, "is-model-finder").contains("value=\"engine\" selected"));
}

#[tokio::test(flavor = "current_thread")]
async fn a_refused_write_shows_the_servers_message_and_keeps_the_old_value() {
    let fake = Fake::new();
    fake.refuse.set(Some("the finder must be agent or engine"));
    let mut screen = mounted(&fake, false).await;
    screen.pick("is-model-finder", "engine");
    screen.pump(300).await;
    let html = screen.html();
    assert!(
        html.contains("the finder must be agent or engine"),
        "{html}"
    );
    assert!(
        select_of(&html, "is-model-finder").contains("value=\"agent\" selected"),
        "the old value is still what the select shows: {html}"
    );
    assert!(!html.contains(">Saved<"), "{html}");
}

#[tokio::test(flavor = "current_thread")]
async fn a_run_in_flight_locks_the_selects_and_a_pick_writes_nothing() {
    let fake = Fake::new();
    let mut screen = mounted(&fake, true).await;
    let html = screen.html();
    assert!(html.contains(RUN_IN_FLIGHT_NOTE), "{html}");
    for chunk in html.split("<select").skip(1) {
        let tag = &chunk[..chunk.find('>').unwrap()];
        assert!(tag.contains("disabled"), "a select is still live: {tag}");
    }
    screen.pick("is-model-group_review", "claude-sonnet-5-5");
    screen.pump(200).await;
    assert!(
        fake.writes.borrow().is_empty(),
        "a locked pick writes nothing"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn showing_the_hidden_rows_is_a_real_click_and_writes_nothing() {
    let fake = Fake::new();
    let mut screen = mounted(&fake, false).await;
    assert!(screen
        .html()
        .contains("Show the 2 rows the Engine finder uses"));
    screen.click("is-reveal-unused");
    screen.pump(200).await;
    let html = screen.html();
    assert!(html.contains("id=\"is-model-audit\""), "{html}");
    assert!(html.contains("not used by the Agent finder"), "{html}");
    assert!(fake.writes.borrow().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn clearing_group_review_sends_a_blank_model_and_the_row_names_the_default() {
    let fake = Fake::new();
    let mut screen = mounted(&fake, false).await;
    screen.pick("is-model-group_review", "");
    screen.pump(300).await;
    assert_eq!(
        *fake.writes.borrow(),
        [("p-1".to_string(), "group_review".to_string(), String::new())]
    );
    let html = screen.html();
    assert!(
        select_of(&html, "is-model-group_review").contains("value=\"\" selected"),
        "{html}"
    );
}

#[test]
fn an_embedded_control_that_saved_moves_the_project_the_derived_lines_read() {
    let mut project: ProjectView = serde_json::from_value(new_project_json()).unwrap();
    assert!(project.client_engagement);
    project = with_event(project, &InspectionEvent::BackendSaved(ProjectBackend::Cli));
    assert_eq!(project.backend, ProjectBackend::Cli);
    project = with_event(project, &InspectionEvent::EngagementSaved(false, false));
    assert!(!project.client_engagement);
    let after = with_event(project.clone(), &InspectionEvent::ShowUnused);
    assert!(after == project, "showing rows changes nothing");
}

// ── The acceptance test: the real inspection start screen over a stub server ────────────

/// The stub server's project: a stateful responder, so a saved value reads back. wiremock needs
/// Send + Sync, so the state is a mutex over the JSON.
#[derive(Clone)]
struct SharedProject(std::sync::Arc<std::sync::Mutex<Value>>);

struct StepModelsReply(SharedProject);

impl Respond for StepModelsReply {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let mut project = self.0 .0.lock().unwrap();
        apply_step(
            &mut project,
            body["step"].as_str().unwrap(),
            body["model"].as_str().unwrap(),
        );
        ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "project": *project }))
    }
}

/// `POST /api/projects/:id/backend`: stores the backend and echoes the project.
struct BackendReply(SharedProject);

impl Respond for BackendReply {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let mut project = self.0 .0.lock().unwrap();
        project["backend"] = body["backend"].clone();
        ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "project": *project }))
    }
}

/// `POST /api/projects/:id/client-engagement`: stores both flags and echoes the project.
struct EngagementReply(SharedProject);

impl Respond for EngagementReply {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let mut project = self.0 .0.lock().unwrap();
        project["client_engagement"] = body["client_engagement"].clone();
        project["client_tooling_opt_in"] = body["client_tooling_opt_in"].clone();
        ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "project": *project }))
    }
}

/// The stub routes the embedded Backend and Client engagement controls write through.
async fn mount_where_it_runs_stubs(server: &MockServer, shared: &SharedProject) {
    Mock::given(method("POST"))
        .and(path("/api/projects/p-1/backend"))
        .respond_with(BackendReply(shared.clone()))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p-1/client-engagement"))
        .respond_with(EngagementReply(shared.clone()))
        .mount(server)
        .await;
}

struct ActiveReply(SharedProject);

impl Respond for ActiveReply {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(self.0 .0.lock().unwrap().clone())
    }
}

fn start_screen() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<super::super::ScanReportView>));
    use_context_provider(|| Signal::new(super::super::CockpitView::Stories));
    use_context_provider(|| Signal::new(None::<String>));
    use_context_provider(|| Signal::new(0_usize) as crate::loading::LoadingCount);
    rsx! { super::super::scan::OnboardView { connection: None } }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_opus_recipe_is_set_on_the_inspection_start_screen_before_any_run() {
    let server = MockServer::start().await;
    let shared = SharedProject(std::sync::Arc::new(std::sync::Mutex::new(
        new_project_json(),
    )));
    Mock::given(method("GET"))
        .and(path("/api/projects/active"))
        .respond_with(ActiveReply(shared.clone()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p-1/step-models"))
        .respond_with(StepModelsReply(shared.clone()))
        .mount(&server)
        .await;
    mount_where_it_runs_stubs(&server, &shared).await;
    Mock::given(method("GET"))
        .and(path("/api/models/registry"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                { "id": "claude-opus-5-5", "display": "Claude Opus 5.5", "provider": "claude" },
                { "id": "claude-sonnet-5-5", "display": "Claude Sonnet 5.5", "provider": "claude" },
                { "id": "claude-haiku-5-5", "display": "Claude Haiku 5.5", "provider": "claude" }
            ]
        })))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

    let mut screen = Screen::mount(start_screen);
    screen.pump(800).await;
    let html = screen.html();
    // Before any run: the settings are on the start screen, from the project alone.
    assert!(
        html.contains("Choose rules, inspect, then review and export."),
        "the start screen\n{html}"
    );
    assert!(html.contains("Finder and models"), "{html}");
    assert!(
        select_of(&html, "is-model-finder").contains("value=\"agent\" selected"),
        "a new project starts on the Agent finder: {html}"
    );
    assert!(
        !server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .any(|r| r.url.path().contains("review/settings")),
        "the component never reads the review settings route, which refuses before a result"
    );

    // Where it runs: a new project is API with Client engagement on. The recipe is CLI with
    // Client engagement off, set with the real controls.
    screen.click(&format!(
        "{}CLI",
        crate::cockpit::rules::BACKEND_SEG_ID_PREFIX
    ));
    screen.pump(300).await;
    screen.set_checked(crate::cockpit::rules::CLIENT_ENGAGEMENT_ID, false);
    screen.pump(300).await;

    // The operator sets the recipe with the real selects.
    screen.pick("is-model-finder", "agent");
    screen.pick("is-model-finder_model", "claude-opus-5-5");
    screen.pump(300).await;
    screen.pick("is-model-group_review", "claude-opus-5-5");
    screen.pump(300).await;
    screen.pick("is-model-fix", "claude-haiku-5-5");
    screen.pump(300).await;
    screen.pick("is-model-duplicate_check", "claude-sonnet-5-5");
    screen.pump(300).await;

    // The saved project values, as the server holds them.
    {
        let saved = shared.0.lock().unwrap();
        assert_eq!(saved["backend"], json!("cli"));
        assert_eq!(saved["client_engagement"], json!(false));
        assert_eq!(saved["review"]["finder"], json!("agent"));
        assert_eq!(saved["review"]["finder_model"], json!("claude-opus-5-5"));
        assert_eq!(
            saved["review"]["group_review_model"],
            json!("claude-opus-5-5")
        );
        assert_eq!(saved["step_models"]["fix"], json!("claude-haiku-5-5"));
        assert_eq!(
            saved["review"]["dup_check_model"],
            json!("claude-sonnet-5-5")
        );
    }
    // And the rows the operator sees.
    let html = screen.html();
    assert!(
        select_of(&html, "is-model-finder_model").contains("value=\"claude-opus-5-5\" selected"),
        "{html}"
    );
    assert!(
        select_of(&html, "is-model-group_review").contains("value=\"claude-opus-5-5\" selected"),
        "{html}"
    );
    assert!(
        select_of(&html, "is-model-fix").contains("value=\"claude-haiku-5-5\" selected"),
        "{html}"
    );
    assert!(
        select_of(&html, "is-model-duplicate_check")
            .contains("value=\"claude-sonnet-5-5\" selected"),
        "{html}"
    );
    assert!(html.contains("Saved"), "{html}");

    // Switching to the Engine finder swaps the rows, with the same real select.
    screen.pick("is-model-finder", "engine");
    screen.pump(300).await;
    let html = screen.html();
    assert!(html.contains("id=\"is-model-audit\""), "{html}");
    assert!(!html.contains("id=\"is-model-finder_model\""), "{html}");
    assert_eq!(
        shared.0.lock().unwrap()["review"]["finder"],
        json!("engine")
    );
}

// ── Settings > This project mounts the same component ───────────────────────────────────

fn settings_page() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<String>));
    crate::loading::provide_loading_context();
    crate::provider_safety::provide_provider_policy_context();
    rsx! { super::super::SettingsView { global_only: false } }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn settings_this_project_shows_the_one_component_once_with_the_projects_values() {
    let server = MockServer::start().await;
    let shared = SharedProject(std::sync::Arc::new(std::sync::Mutex::new(
        new_project_json(),
    )));
    Mock::given(method("GET"))
        .and(path("/api/projects/active"))
        .respond_with(ActiveReply(shared.clone()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p-1/step-models"))
        .respond_with(StepModelsReply(shared.clone()))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = Screen::mount(settings_page);
    screen.pump(800).await;
    let html = screen.html();
    assert!(html.contains("This project"), "{html}");
    assert!(html.contains("Finder and models"), "{html}");
    assert_eq!(
        html.matches("id=\"is-model-finder\"").count(),
        1,
        "the component is mounted once, not beside a second copy of its controls"
    );
    assert_eq!(
        html.matches("Project backend").count(),
        1,
        "one Backend control, the component's"
    );
    // The real select writes the project, here as in the other two mounts.
    screen.pick("is-model-finder", "engine");
    screen.pump(300).await;
    assert_eq!(
        shared.0.lock().unwrap()["review"]["finder"],
        json!("engine")
    );
}

// ── Round 2: the conflict lines follow the real controls ────────────────────────────────

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_cli_with_client_engagement_line_follows_the_real_backend_and_engagement_controls() {
    let server = MockServer::start().await;
    let shared = SharedProject(std::sync::Arc::new(std::sync::Mutex::new(
        new_project_json(),
    )));
    mount_where_it_runs_stubs(&server, &shared).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let fake = Fake::new();
    let mut screen = mounted(&fake, false).await;
    assert!(
        !screen.html().contains("inspection-settings-conflict"),
        "API with engagement: no conflict"
    );
    // Click CLI with Client engagement still on.
    screen.click(&format!(
        "{}CLI",
        crate::cockpit::rules::BACKEND_SEG_ID_PREFIX
    ));
    screen.pump(400).await;
    let html = screen.html();
    assert!(
        html.contains("A client engagement runs on the API only."),
        "{html}"
    );
    // Untick Client engagement: the line goes.
    screen.set_checked(crate::cockpit::rules::CLIENT_ENGAGEMENT_ID, false);
    screen.pump(400).await;
    let html = screen.html();
    assert!(
        !html.contains("A client engagement runs on the API only."),
        "{html}"
    );
    assert_eq!(shared.0.lock().unwrap()["backend"], json!("cli"));
    assert_eq!(shared.0.lock().unwrap()["client_engagement"], json!(false));
}

#[test]
fn a_non_anthropic_model_on_a_client_engagement_shows_a_one_line_warning() {
    #[derive(Clone, PartialEq, Props)]
    struct Case {
        project: ProjectView,
    }
    #[component]
    fn Shown(props: Case) -> Element {
        use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
        rsx! { InspectionSettingsView { project: props.project, on_event: move |_| {} } }
    }
    let make = |engagement: bool| {
        let mut p = new_project_json();
        p["review"]["group_review_model"] = json!("meta/llama-3:free");
        p["client_engagement"] = json!(engagement);
        p["backend"] = json!("api");
        crate::test_harness::render_fragment(
            Shown,
            Case {
                project: serde_json::from_value(p).unwrap(),
            },
        )
    };
    let on = make(true);
    assert!(
        on.contains(
            "This project is a client engagement: only Anthropic models may run. Group review model is set to meta/llama-3:free."
        ),
        "{on}"
    );
    assert!(!make(false).contains("only Anthropic models may run"));
}

#[test]
fn the_fix_row_says_what_runs_when_it_is_not_set() {
    let html = page("all-unset");
    assert!(
        html.contains("or the calibration model when the Haiku fix switch is off"),
        "{html}"
    );
}
