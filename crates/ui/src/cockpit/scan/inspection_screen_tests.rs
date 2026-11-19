//! The Inspection screen tells the truth (settings overhaul, work order C): the real inspection
//! screen (`OnboardView` with `ScanResults` inside it) mounted over a stub server, driven with the
//! operator's real controls. Per finder: which rows the "This run" block shows, what the estimate
//! says, what the Inspect button sends, and that every long explanation sits behind a "?" button.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dioxus::dioxus_core::{AttributeValue, ElementId, Mutation, Mutations};
use dioxus::html::{Code, Key, Location, Modifiers, SerializedKeyboardData};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::*;

// ── The mounted screen and what it has told us about its elements ─────────────────────────────

struct Screen {
    vdom: VirtualDom,
    by_html_id: HashMap<String, ElementId>,
    tours: Vec<(String, ElementId)>,
    listeners: Vec<(String, ElementId)>,
}

impl Screen {
    fn mount(app: fn() -> Element) -> Self {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let mut vdom = VirtualDom::new(app);
        let mut screen = Self {
            vdom: VirtualDom::new(|| rsx! {}),
            by_html_id: HashMap::new(),
            tours: Vec::new(),
            listeners: Vec::new(),
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
                Mutation::SetAttribute {
                    name: "data-tour",
                    value: AttributeValue::Text(text),
                    id,
                    ..
                } => self.tours.push((text.clone(), *id)),
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

    fn id_of(&self, html_id: &str) -> ElementId {
        *self
            .by_html_id
            .get(html_id)
            .unwrap_or_else(|| panic!("no element {html_id}: {:?}", self.by_html_id.keys()))
    }

    fn pick(&self, html_id: &str, value: &str) {
        let id = self.id_of(html_id);
        assert!(
            self.listeners.iter().any(|(n, i)| n == "change" && *i == id),
            "{html_id} has no change listener"
        );
        let data: Rc<dyn std::any::Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedFormData::new(value.to_string(), Vec::new()),
        )));
        self.vdom
            .runtime()
            .handle_event("change", dioxus::dioxus_core::Event::new(data, true), id);
    }

    fn click_id(&self, id: ElementId) {
        let data: Rc<dyn std::any::Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedMouseData::default(),
        )));
        self.vdom
            .runtime()
            .handle_event("click", dioxus::dioxus_core::Event::new(data, true), id);
    }

    fn click(&self, html_id: &str) {
        self.click_id(self.id_of(html_id));
    }

    fn click_tour(&self, tour: &str) {
        let id = self
            .tours
            .iter()
            .rev()
            .find(|(t, _)| t == tour)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no element marked {tour}: {:?}", self.tours));
        self.click_id(id);
    }

    fn escape(&self, html_id: &str) {
        let data: Rc<dyn std::any::Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            SerializedKeyboardData::new(
                Key::Escape,
                Code::Escape,
                Location::Standard,
                false,
                Modifiers::empty(),
                false,
            ),
        )));
        self.vdom.runtime().handle_event(
            "keydown",
            dioxus::dioxus_core::Event::new(data, true),
            self.id_of(html_id),
        );
    }
}

// ── The stub server ─────────────────────────────────────────────────────────────────────────

#[derive(Clone)]
struct SharedProject(Arc<Mutex<Value>>);

fn project_json(finder: &str) -> Value {
    json!({
        "id": "p-1", "name": "Hearth", "repos": ["me/api"],
        "review": serde_json::to_value(
            camerata_api_types::project::ReviewSettings::for_new_project()
        ).map(|mut v| { v["finder"] = json!(finder); v }).unwrap(),
        "step_models": serde_json::to_value(camerata_api_types::project::StepModels::default()).unwrap(),
        "backend": "api", "client_engagement": true, "client_name": "",
    })
}

struct ActiveReply(SharedProject);

impl Respond for ActiveReply {
    fn respond(&self, _request: &Request) -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_json(self.0 .0.lock().unwrap().clone())
    }
}

struct StepModelsReply(SharedProject);

impl Respond for StepModelsReply {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let mut project = self.0 .0.lock().unwrap();
        let (section, key) = match body["step"].as_str().unwrap() {
            "finder" => ("review", "finder"),
            "finder_model" => ("review", "finder_model"),
            "audit" => ("step_models", "audit"),
            "calibration" => ("step_models", "calibration"),
            "fix" => ("step_models", "fix"),
            "group_review" => ("review", "group_review_model"),
            "duplicate_check" => ("review", "dup_check_model"),
            "clone_judge" => ("review", "clone_judge_model"),
            other => panic!("unknown step {other}"),
        };
        project[section][key] = body["model"].clone();
        ResponseTemplate::new(200).set_body_json(json!({ "ok": true, "project": *project }))
    }
}

/// What the preflight route answers for the one repo.
#[derive(Clone, Copy)]
enum Sizes {
    /// The repo, with the Agent finder's figures.
    Back,
    /// The server fails.
    Failed,
    /// The answer takes longer than the test waits.
    Slow,
}

async fn stub_server(finder: &str, sizes: Sizes) -> (MockServer, SharedProject) {
    let server = MockServer::start().await;
    let shared = SharedProject(Arc::new(Mutex::new(project_json(finder))));
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
    Mock::given(method("GET"))
        .and(path("/api/models/registry"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "models": [
                { "id": "claude-opus-5-5", "display": "Claude Opus 5.5", "provider": "claude",
                  "price_in": 5.0, "price_out": 25.0 },
                { "id": "claude-sonnet-5-5", "display": "Claude Sonnet 5.5", "provider": "claude",
                  "price_in": 3.0, "price_out": 15.0 },
                { "id": "claude-haiku-5-5", "display": "Claude Haiku 5.5", "provider": "claude",
                  "price_in": 1.0, "price_out": 5.0 }
            ]
        })))
        .mount(&server)
        .await;
    for p in ["/api/onboard/draft"] {
        Mock::given(method("POST"))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "ok": true })))
            .mount(&server)
            .await;
    }
    let preflight = match sizes {
        Sizes::Back => ResponseTemplate::new(200).set_body_json(json!([{
            "repo": "me/api", "files": 10, "code_chars": 4000,
            "agent_finder": {
                "pieces": 3, "lines": 900, "joints_calls": 1,
                "expected_usd": 4.2, "low_usd": 2.52, "high_usd": 7.56
            }
        }])),
        Sizes::Failed => ResponseTemplate::new(500),
        Sizes::Slow => ResponseTemplate::new(200)
            .set_delay(Duration::from_secs(30))
            .set_body_json(json!([])),
    };
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(preflight)
        .mount(&server)
        .await;
    (server, shared)
}

async fn mount_job(server: &MockServer, status: &str) {
    Mock::given(method("POST"))
        .and(path("/api/onboard/audit/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "job_id": "job-9" })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/audit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "repos": ["me/api"], "files_scanned": 1, "findings": [],
            "proposed_rules": [], "gated": false
        })))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/audit/job/job-9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "job": {
                "status": status, "done": 0, "total": 1, "findings": [],
                "report": null, "message": null
            },
            "idle_ms": null, "cancel_requested": false
        })))
        .mount(server)
        .await;
}

fn the_report() -> ScanReportView {
    serde_json::from_value(json!({
        "repos": ["me/api"], "files_scanned": 10, "code_chars": 4000, "findings": [],
        "gated": false,
        "proposed_rules": [{
            "id": "R-1", "title": "No raw SQL", "kind": "semantic", "scope": "repo-local",
            "repos": ["me/api"], "is_auto_recommended": true, "recommended": true,
            "verification": "grounded"
        }]
    }))
    .expect("a scan with one pre-selected rule")
}

fn providers(scan: Option<ScanReportView>) {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(move || Signal::new(scan));
    use_context_provider(|| Signal::new(CockpitView::Stories));
    use_context_provider(|| Signal::new(None::<String>));
    use_context_provider(|| Signal::new(0_usize) as crate::loading::LoadingCount);
}

/// The inspection screen once rules are proposed.
fn rules_proposed() -> Element {
    providers(Some(the_report()));
    rsx! { OnboardView { connection: None } }
}

/// The inspection screen before any scan.
fn start_screen() -> Element {
    providers(None);
    rsx! { OnboardView { connection: None } }
}

// ── Reading the page ────────────────────────────────────────────────────────────────────────

/// The visible text of some markup: tags removed, entities for the few characters the renderer
/// escapes put back.
fn visible_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => {
                in_tag = true;
                out.push(' ');
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
}

/// The part of the page from the element with html id `id` to the end of its section: up to the
/// next element that starts one of the screen's later blocks.
fn block(html: &str, id: &str) -> String {
    let start = html
        .find(&format!("id=\"{id}\""))
        .unwrap_or_else(|| panic!("no block {id} in {html}"));
    let rest = &html[start..];
    let end = ["class=\"findings-toolbar\"", "class=\"job-progress\"", "class=\"audit-thinking\""]
        .iter()
        .filter_map(|m| rest.find(m))
        .min()
        .unwrap_or(rest.len());
    rest[..end].to_string()
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

async fn requests_to(server: &MockServer, route: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == route)
        .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
        .collect()
}

const RUN: &str = "inspection-run";

// ── The Agent finder ────────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn an_agent_project_shows_no_model_rows_a_locked_full_inspection_no_run_mode_and_its_own_estimate(
) {
    let (server, _) = stub_server("agent", Sizes::Back).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = Screen::mount(rules_proposed);
    screen.pump(900).await;
    let html = screen.html();
    let run = block(&html, RUN);
    let text = visible_text(&run);

    // The per-run model picks are gone: no select in the block but Checks to run, and none of
    // the words that named them.
    assert_eq!(run.matches("<select").count(), 1, "only Checks to run: {run}");
    assert!(run.contains("id=\"inspection-checks\""), "{run}");
    for gone in ["Audit model", "Calibration model", "Default = the scan model"] {
        assert!(!text.contains(gone), "{gone}: {text}");
    }
    // What to scan is Checks to run now.
    assert!(text.contains("Checks to run"), "{text}");
    assert!(!text.contains("What to scan"), "{text}");
    assert!(!text.contains("AI architectural review"), "{text}");
    // No Run mode, no Scan mode, for the Agent finder.
    assert!(!text.contains("Run mode") && !text.contains("Scan mode"), "{text}");
    assert!(!run.contains("id=\"inspection-run-mode\""), "{run}");
    // Full inspection: ticked and locked, with the one line that says why.
    assert!(text.contains("Full inspection") && !text.contains("Full scan"), "{text}");
    let full = tag_of(&run, "inspection-full");
    assert!(full.contains("checked") && full.contains("disabled"), "{full}");
    assert!(text.contains("The Agent finder always inspects every file."), "{text}");
    assert!(!text.contains("INCREMENTAL"), "{text}");
    // The estimate is the Agent finder's: the finder stage, pieces and joints calls, and the
    // line that says the rest is extra. Never the Engine's figure, tokens, passes or rule count.
    let estimate = visible_text(&block(&html, "inspection-estimate"));
    assert!(estimate.contains("Estimated cost of the finder stage"), "{estimate}");
    assert!(estimate.contains("about $4.20"), "{estimate}");
    assert!(estimate.contains("most likely between $2.52 and $7.56"), "{estimate}");
    assert!(estimate.contains("3 pieces, 1 joints call"), "{estimate}");
    assert!(estimate.contains("Other stages are extra."), "{estimate}");
    for engine in ["pass(es)", "rule(s)", "tokens"] {
        assert!(!estimate.contains(engine), "{engine}: {estimate}");
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn an_agent_estimate_that_cannot_be_made_says_so_and_never_shows_the_engine_figure() {
    for sizes in [Sizes::Failed, Sizes::Slow] {
        let (server, _) = stub_server("agent", sizes).await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let mut screen = Screen::mount(rules_proposed);
        screen.pump(900).await;
        let html = screen.html();
        let estimate = visible_text(&block(&html, "inspection-estimate"));
        assert!(
            estimate.contains("Estimate not available for the Agent finder"),
            "{estimate}"
        );
        assert!(!estimate.contains('$'), "no dollar figure: {estimate}");
        assert!(
            !estimate.contains("pass(es)") && !estimate.contains("rule(s)"),
            "no Engine figure under the Agent finder: {estimate}"
        );
    }
}

// ── The Engine finder ───────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn an_engine_project_shows_run_mode_a_free_full_inspection_and_its_own_estimate() {
    let (server, _) = stub_server("engine", Sizes::Back).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = Screen::mount(rules_proposed);
    screen.pump(900).await;
    let html = screen.html();
    let run = block(&html, RUN);
    let text = visible_text(&run);
    assert_eq!(run.matches("<select").count(), 2, "Checks to run and Run mode: {run}");
    assert!(text.contains("Run mode") && !text.contains("Scan mode"), "{text}");
    assert!(run.contains("id=\"inspection-run-mode\""), "{run}");
    for gone in ["Audit model", "Default = the scan model"] {
        assert!(!text.contains(gone), "{gone}: {text}");
    }
    // The mode's options say what is true: every inspection is a saved job.
    assert!(text.contains("Large inspection"), "{text}");
    assert!(!text.contains("Large scan"), "{text}");
    let full = tag_of(&run, "inspection-full");
    assert!(!full.contains("disabled") && !full.contains("checked"), "{full}");
    assert!(!text.contains("The Agent finder always inspects every file."), "{text}");
    let estimate = visible_text(&block(&html, "inspection-estimate"));
    assert!(estimate.contains("Estimated cost"), "{estimate}");
    assert!(estimate.contains("pass(es)") && estimate.contains("rule(s)"), "{estimate}");
    assert!(!estimate.contains("finder stage"), "{estimate}");
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn deterministic_checks_only_costs_nothing_for_both_finders() {
    for finder in ["agent", "engine"] {
        let (server, _) = stub_server(finder, Sizes::Back).await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let mut screen = Screen::mount(rules_proposed);
        screen.pump(900).await;
        screen.pick("inspection-checks", "deterministic");
        screen.pump(200).await;
        let html = screen.html();
        let estimate = visible_text(&block(&html, "inspection-estimate"));
        assert!(estimate.contains("$0, no model calls"), "{finder}: {estimate}");
        assert!(!estimate.contains("pass(es)"), "{finder}: {estimate}");
        assert!(!estimate.contains("finder stage"), "{finder}: {estimate}");
    }
}

// ── The Inspect button ──────────────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_inspect_button_starts_a_saved_job_with_no_model_and_its_label_changes_while_it_runs() {
    let (server, _) = stub_server("agent", Sizes::Back).await;
    mount_job(&server, "running").await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = Screen::mount(rules_proposed);
    screen.pump(900).await;
    let before = visible_text(&screen.html());
    assert!(before.contains("Inspect code against selected rules"), "{before}");
    assert!(!before.contains("Audit code against selected rules"), "{before}");

    screen.click_tour("run-inspection");
    screen.pump(1500).await;

    let started = requests_to(&server, "/api/onboard/audit/start").await;
    assert_eq!(started.len(), 1, "one saved job was started");
    assert!(
        requests_to(&server, "/api/onboard/audit").await.is_empty(),
        "the plain request path was not used"
    );
    let body = &started[0];
    assert!(body.get("model").is_none(), "no model is sent: {body}");
    assert!(body.get("calibration_model").is_none(), "no calibration model is sent: {body}");
    assert_eq!(body["incremental"], json!(false), "the Agent finder inspects every file: {body}");
    assert_eq!(body["run_ai_review"], json!(true));
    assert_eq!(body["run_deterministic"], json!(true));
    // While it runs the button reads Inspecting and the old label is gone.
    let running = visible_text(&screen.html());
    assert!(running.contains("Inspecting"), "{running}");
    assert!(!running.contains("Auditing"), "{running}");
    assert!(!running.contains("Inspect code against selected rules"), "{running}");
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn an_engine_run_sends_the_pickers_it_still_has_and_never_a_model() {
    let (server, _) = stub_server("engine", Sizes::Back).await;
    mount_job(&server, "running").await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = Screen::mount(rules_proposed);
    screen.pump(900).await;
    screen.pick("inspection-run-mode", "parallel");
    screen.pick("inspection-checks", "ai");
    screen.pump(200).await;
    screen.click_tour("run-inspection");
    screen.pump(1500).await;
    let started = requests_to(&server, "/api/onboard/audit/start").await;
    assert_eq!(started.len(), 1);
    let body = &started[0];
    assert!(body.get("model").is_none() && body.get("calibration_model").is_none(), "{body}");
    assert_eq!(body["mode"], json!("parallel"));
    assert_eq!(body["incremental"], json!(true), "an Engine run is incremental unless ticked");
    assert_eq!(body["run_ai_review"], json!(true));
    assert_eq!(body["run_deterministic"], json!(false));
}

// ── The finder changes on screen ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn changing_the_finder_redraws_the_run_block_and_the_estimate_without_a_remount() {
    let (server, shared) = stub_server("agent", Sizes::Back).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = Screen::mount(rules_proposed);
    screen.pump(900).await;
    assert!(!screen.html().contains("id=\"inspection-run-mode\""));
    assert!(visible_text(&block(&screen.html(), "inspection-estimate"))
        .contains("Estimated cost of the finder stage"));

    // The operator switches the finder in the Inspection settings on this same screen.
    screen.pick("is-model-finder", "engine");
    screen.pump(900).await;
    assert_eq!(shared.0.lock().unwrap()["review"]["finder"], json!("engine"));
    let html = screen.html();
    assert!(html.contains("id=\"inspection-run-mode\""), "Run mode appears: {html}");
    let full = tag_of(&block(&html, RUN), "inspection-full");
    assert!(!full.contains("disabled"), "Full inspection is free again: {full}");
    let estimate = visible_text(&block(&html, "inspection-estimate"));
    assert!(estimate.contains("pass(es)"), "the Engine figure: {estimate}");
    assert!(!estimate.contains("finder stage"), "{estimate}");

    // And back: the Agent finder's block returns.
    screen.pick("is-model-finder", "agent");
    screen.pump(900).await;
    let html = screen.html();
    assert!(!html.contains("id=\"inspection-run-mode\""), "{html}");
    assert!(visible_text(&block(&html, "inspection-estimate"))
        .contains("Estimated cost of the finder stage"));
}

// ── The settings are on the screen after rules are proposed too ─────────────────────────────

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_inspection_settings_stay_on_the_screen_once_rules_are_proposed_with_one_client_name() {
    let (server, _) = stub_server("agent", Sizes::Back).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = Screen::mount(rules_proposed);
    screen.pump(900).await;
    let html = screen.html();
    assert!(html.contains("Finder and models"), "{html}");
    assert!(html.contains("id=\"is-model-finder\""), "{html}");
    assert_eq!(
        html.matches("id=\"inspection-client-name\"").count(),
        1,
        "exactly one Client name field"
    );
    // The same before any scan.
    let mut start = Screen::mount(start_screen);
    start.pump(900).await;
    let html = start.html();
    assert!(html.contains("Finder and models"), "{html}");
    assert_eq!(html.matches("id=\"inspection-client-name\"").count(), 1);
}

// ── Help behind "?" ─────────────────────────────────────────────────────────────────────────

/// The "?" buttons of the run block and, for each, a sentence only its dialog holds.
const AGENT_HELP: [(&str, &str, &str); 5] = [
    ("inspect", "About This run", "each a live conversation"),
    ("checks-to-run", "About Checks to run", "Deterministic checks only runs"),
    ("full-inspection", "About Full inspection", "ticked and locked for it"),
    ("spend-limit", "About Spend limit", "Work already started finishes"),
    ("estimate", "About The cost estimate", "at fixed rates taken from earlier runs"),
];

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_long_explanations_are_behind_question_mark_buttons_that_open_and_close() {
    let (server, _) = stub_server("agent", Sizes::Back).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = Screen::mount(rules_proposed);
    screen.pump(900).await;
    let page = screen.html();
    // The long text is not on the page.
    for long in [
        "Message Batches API",
        "INCREMENTAL",
        "ignore the incremental cache",
        "Passes already started finish",
        "Deterministic-only: the always-on security floor",
        "Approximate",
        "each a live conversation",
    ] {
        assert!(!page.contains(long), "{long} is still inline");
    }
    for (topic, label, sentence) in AGENT_HELP {
        assert!(
            page.contains(&format!("aria-label=\"{label}\"")),
            "{label} button: {page}"
        );
        let button = format!("help-btn-{topic}");
        let dialog = format!("help-dialog-{topic}");
        screen.click(&button);
        screen.pump(100).await;
        let open = screen.html();
        assert!(open.contains("role=\"dialog\""), "{label} opens a dialog");
        assert!(open.contains(sentence), "{label} dialog holds its text: {sentence}");
        screen.escape(&dialog);
        screen.pump(100).await;
        let closed = screen.html();
        assert!(!closed.contains("role=\"dialog\""), "{label}: Esc closes it");
        assert!(
            closed.contains(&format!("id=\"{button}\"")),
            "{label}: the button is there for focus to return to"
        );
    }
    assert!(!page.contains("aria-label=\"About Run mode\""), "no Run mode for the Agent finder");
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn run_mode_has_its_own_help_button_for_the_engine_finder() {
    let (server, _) = stub_server("engine", Sizes::Back).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = Screen::mount(rules_proposed);
    screen.pump(900).await;
    let page = screen.html();
    assert!(page.contains("aria-label=\"About Run mode\""), "{page}");
    for long in ["Message Batches API", "flat 50% discount", "Sequential runs"] {
        assert!(!page.contains(long), "{long} is still inline");
    }
    screen.click("help-btn-run-mode");
    screen.pump(100).await;
    assert!(screen.html().contains("flat 50% discount"));
    screen.escape("help-dialog-run-mode");
    screen.pump(100).await;
    assert!(!screen.html().contains("role=\"dialog\""));
}

// ── The words ───────────────────────────────────────────────────────────────────────────────

fn whole_word_audit(text: &str) -> bool {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .any(|w| w == "audit")
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn no_string_the_inspection_screen_renders_says_audit_or_uses_a_dash_or_a_serial_comma() {
    for finder in ["agent", "engine"] {
        let (server, _) = stub_server(finder, Sizes::Back).await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let mut screen = Screen::mount(rules_proposed);
        screen.pump(900).await;
        let text = visible_text(&screen.html());
        assert!(!whole_word_audit(&text), "{finder}: {text}");
        let run = visible_text(&block(&screen.html(), RUN));
        assert!(!run.contains(['\u{2013}', '\u{2014}']), "{finder}: {run}");
        assert!(!run.contains(", and "), "{finder}: {run}");
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_start_screen_is_titled_inspection_and_its_button_says_propose_rules() {
    let (server, _) = stub_server("agent", Sizes::Back).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = Screen::mount(start_screen);
    screen.pump(900).await;
    let text = visible_text(&screen.html());
    assert!(text.contains("Inspection"), "{text}");
    assert!(!text.contains("Onboard repos"), "{text}");
    assert!(text.contains("Propose rules") && !text.contains("Scan repos"), "{text}");
    assert!(!whole_word_audit(&text), "{text}");
}

fn nav() -> Element {
    use_context_provider(|| Signal::new(CockpitScreen::Projects));
    let view = use_signal(|| CockpitView::Onboard);
    rsx! { super::super::CockpitNav { view } }
}

#[test]
fn the_navigation_item_reads_inspection() {
    let mut vdom = VirtualDom::new(nav);
    vdom.rebuild_in_place();
    let text = visible_text(&dioxus_ssr::render(&vdom));
    assert!(text.contains("Inspection"), "{text}");
    assert!(!text.contains("Onboard repos"), "{text}");
}
