//! "Run what remains" after a scan started from the screen (Zach's hearth-sample run, 2026-11-18):
//! the button was off ("This scan was not saved") for a run that WAS saved, because a scan run
//! with Scan mode Default went down the plain request path, which saves no run. Every scan is a
//! saved job now. These tests mount the real results screen and the real banner against a stub
//! server.

use super::*;
use dioxus::dioxus_core::{ElementId, Mutation, Mutations};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn incomplete_scan(run_id: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "repos": ["me/api"], "files_scanned": 1, "findings": [],
        "proposed_rules": [], "gated": false, "run_id": run_id,
        "failed_passes": [{
            "repo": "me/api", "pass": "fix-specific generation", "reason": "the run stopped",
            "class": "spend_limit"
        }]
    })
}

fn results() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<ScanReportView>));
    use_context_provider(|| Signal::new(CockpitView::Stories));
    use_context_provider(|| Signal::new(None::<String>));
    use_context_provider(|| Signal::new(0_usize) as crate::loading::LoadingCount);
    let report: ScanReportView = serde_json::from_value(serde_json::json!({
        "repos": ["me/api"], "files_scanned": 1, "findings": [],
        "proposed_rules": [], "gated": false
    }))
    .expect("the scan the draft belongs to");
    rsx! { ScanResults { report } }
}

async fn pump(vdom: &mut VirtualDom, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        let _ = vdom.render_immediate_to_vec();
    }
}

async fn stub_screen(server: &MockServer, audit: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "scan": {
                "repos": ["me/api"], "files_scanned": 1, "findings": [],
                "proposed_rules": [], "gated": false
            },
            "audit": audit,
        })))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/active"))
        .respond_with(ResponseTemplate::new(404))
        .mount(server)
        .await;
}

fn button_html(html: &str) -> String {
    let after = html
        .split("scan-run-what-remains")
        .nth(1)
        .unwrap_or_else(|| panic!("no Run what remains button: {html}"));
    after.split("</button>").next().unwrap().to_string()
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_screen_enables_run_what_remains_for_a_saved_scan_and_keeps_it_off_for_an_unsaved_one()
{
    // A saved run: the button is live.
    let server = MockServer::start().await;
    stub_screen(&server, incomplete_scan(Some("20261118T101500Z-ab12cd"))).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut vdom = VirtualDom::new(results);
    vdom.rebuild_in_place();
    pump(&mut vdom, 800).await;
    let button = button_html(&dioxus_ssr::render(&vdom));
    assert!(!button.contains("disabled"), "{button}");
    assert!(
        button.contains("Nothing is sent until you confirm"),
        "{button}"
    );
    assert!(!button.contains("was not saved"), "{button}");
    drop(_env);

    // A scan that was not saved: the button stays off and says why.
    let server = MockServer::start().await;
    stub_screen(&server, incomplete_scan(None)).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut vdom = VirtualDom::new(results);
    vdom.rebuild_in_place();
    pump(&mut vdom, 800).await;
    let button = button_html(&dioxus_ssr::render(&vdom));
    assert!(button.contains("disabled"), "{button}");
    assert!(
        button.contains("This scan was not saved, so it cannot be resumed"),
        "{button}"
    );
}

// ── the click, on the real banner ───────────────────────────────────────────────────

#[derive(Default)]
struct Listeners(Vec<(String, ElementId)>);

impl Listeners {
    fn take(&mut self, m: &Mutations) {
        for e in &m.edits {
            if let Mutation::NewEventListener { name, id } = e {
                self.0.push((name.clone(), *id));
            }
        }
    }
}

thread_local! {
    static BANNER_RUN: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn banner() -> Element {
    let run_id = BANNER_RUN.with(|r| r.borrow().clone());
    rsx! {
        IncompletePassesBanner {
            failed_passes: vec![FailedPassView {
                repo: "me/api".to_string(),
                pass: "fix-specific generation".to_string(),
                ..FailedPassView::default()
            }],
            compact: false,
            run_id,
            on_plan: EventHandler::new(|id: String| {
                spawn(async move {
                    let _ = resume_plan(&id).await;
                });
            }),
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn clicking_run_what_remains_on_a_saved_scan_fetches_its_plan() {
    use std::any::Any;
    use std::rc::Rc;
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/inspections/run-77/plan"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "text": "Resume: what remains", "refusal": null
        })))
        .expect(1)
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));

    BANNER_RUN.with(|r| *r.borrow_mut() = Some("run-77".to_string()));
    let mut vdom = VirtualDom::new(banner);
    let mut seen = Listeners::default();
    seen.take(&vdom.rebuild_to_vec());
    // The banner has the failed row's copy button and "Run what remains": click each click
    // listener (the copy button only writes the clipboard); the plan is fetched by the second.
    let ids: Vec<ElementId> = seen
        .0
        .iter()
        .filter(|(n, _)| n == "click")
        .map(|(_, id)| *id)
        .collect();
    assert!(!ids.is_empty(), "{:?}", seen.0);
    for id in ids {
        let data: Box<dyn Any> = Box::new(dioxus::html::SerializedMouseData::default());
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime()
            .handle_event("click", dioxus::dioxus_core::Event::new(platform, true), id);
    }
    pump(&mut vdom, 500).await;
    // `expect(1)` on the stub checks the plan was fetched when the server drops.
    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/api/inspections/run-77/plan")
            .count(),
        1
    );
}

#[test]
fn every_picked_scan_mode_runs_as_a_saved_job_and_background_job_is_the_server_default() {
    // Background job is the server's default; any other pick is kept (it decides how the model
    // is called). Nothing here is the plain request path: the screen always starts a job.
    assert_eq!(job_exec_mode("job"), "auto");
    for picked in ["auto", "parallel", "sequential", "batch"] {
        assert_eq!(job_exec_mode(picked), picked);
    }
}

// ── the REAL audit button, Scan mode Default ───────────────────────────────────────

/// What the mounted screen has told us about its elements: every listener, and which element
/// carries each `data-tour` marker (the audit button carries `run-inspection`).
#[derive(Default)]
struct Wired {
    listeners: Vec<(String, ElementId)>,
    tours: Vec<(String, ElementId)>,
}

impl Wired {
    fn take(&mut self, m: &Mutations) {
        for e in &m.edits {
            match e {
                Mutation::NewEventListener { name, id } => self.listeners.push((name.clone(), *id)),
                Mutation::SetAttribute {
                    name: "data-tour",
                    value: dioxus::dioxus_core::AttributeValue::Text(text),
                    id,
                    ..
                } => self.tours.push((text.clone(), *id)),
                _ => {}
            }
        }
    }

    fn click_tour(&self, vdom: &VirtualDom, tour: &str) {
        let id = self
            .tours
            .iter()
            .find(|(t, _)| t == tour)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no element marked {tour}: {:?}", self.tours));
        assert!(
            self.listeners.iter().any(|(n, i)| n == "click" && *i == id),
            "the {tour} element has no click listener"
        );
        let data: Box<dyn std::any::Any> = Box::new(dioxus::html::SerializedMouseData::default());
        let platform: std::rc::Rc<dyn std::any::Any> =
            std::rc::Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime()
            .handle_event("click", dioxus::dioxus_core::Event::new(platform, true), id);
    }
}

async fn pump_wired(vdom: &mut VirtualDom, wired: &mut Wired, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        wired.take(&vdom.render_immediate_to_vec());
    }
}

fn results_with_a_selected_rule() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<ScanReportView>));
    use_context_provider(|| Signal::new(CockpitView::Stories));
    use_context_provider(|| Signal::new(None::<String>));
    use_context_provider(|| Signal::new(0_usize) as crate::loading::LoadingCount);
    let report: ScanReportView = serde_json::from_value(serde_json::json!({
        "repos": ["me/api"], "files_scanned": 1, "findings": [], "gated": false,
        "proposed_rules": [{
            "id": "R-1", "title": "No raw SQL", "kind": "semantic", "scope": "repo-local",
            "repos": ["me/api"], "is_auto_recommended": true, "recommended": true,
            "verification": "grounded"
        }]
    }))
    .expect("a scan with one pre-selected rule");
    rsx! { ScanResults { report } }
}

async fn stub_for_a_scan_run(server: &MockServer) {
    for (m, p) in [
        ("GET", "/api/onboard/draft"),
        ("POST", "/api/onboard/draft"),
    ] {
        Mock::given(method(m))
            .and(path(p))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "scan": {
                    "repos": ["me/api"], "files_scanned": 1, "findings": [],
                    "gated": false, "proposed_rules": []
                }
            })))
            .mount(server)
            .await;
    }
    Mock::given(method("GET"))
        .and(path("/api/projects/active"))
        .respond_with(ResponseTemplate::new(404))
        .mount(server)
        .await;
    // The plain request path: if the screen ever goes here again, it gets a report with NO saved
    // run, exactly what the server's plain handler returns.
    Mock::given(method("POST"))
        .and(path("/api/onboard/audit"))
        .respond_with(ResponseTemplate::new(200).set_body_json(incomplete_scan(None)))
        .mount(server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/audit/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "job_id": "job-9"
        })))
        .mount(server)
        .await;
}

fn job_poll(status: &str, report: serde_json::Value, message: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "job": {
            "status": status, "done": 1, "total": 1, "findings": [],
            "report": report, "message": message
        },
        "idle_ms": null, "cancel_requested": false
    })
}

async fn posts_to(server: &MockServer, route: &str) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == route)
        .count()
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_audit_button_in_default_mode_starts_a_saved_job_and_run_what_remains_is_live() {
    let server = MockServer::start().await;
    // Run mode (the old Scan mode) is the Engine finder's control, so this project is on the
    // Engine finder. The first mock mounted wins, so it comes before the shared stubs.
    Mock::given(method("GET"))
        .and(path("/api/projects/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "p-1", "name": "Hearth", "repos": ["me/api"],
            "review": { "finder": "engine" },
            "backend": "api", "client_engagement": true, "client_name": "",
        })))
        .mount(&server)
        .await;
    stub_for_a_scan_run(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/audit/job/job-9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(job_poll(
            "done",
            incomplete_scan(Some("20261118T101500Z-ab12cd")),
            None,
        )))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));

    let mut vdom = VirtualDom::new(results_with_a_selected_rule);
    let mut wired = Wired::default();
    wired.take(&vdom.rebuild_to_vec());
    pump_wired(&mut vdom, &mut wired, 800).await;
    // The screen is on Run mode Default (one small repo: the recommended mode is `auto`).
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains(
            "<select id=\"inspection-run-mode\" class=\"audit-model-select\" value=\"auto\">"
        ),
        "Run mode is Default: {html}"
    );

    wired.click_tour(&vdom, "run-inspection");
    pump_wired(&mut vdom, &mut wired, 3_000).await;

    assert_eq!(
        posts_to(&server, "/api/onboard/audit/start").await,
        1,
        "one saved job was started"
    );
    assert_eq!(
        posts_to(&server, "/api/onboard/audit").await,
        0,
        "the plain request path (which saves no run) was not used"
    );
    let button = button_html(&dioxus_ssr::render(&vdom));
    assert!(
        !button.contains("disabled"),
        "Run what remains is live: {button}"
    );
    assert!(!button.contains("was not saved"), "{button}");
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_client_lock_refusal_is_shown_as_the_servers_own_refusal_not_a_backend_setup_problem() {
    let lock = "This is a client engagement, so it runs only on the Anthropic API under \
                Anthropic's commercial terms. The `cli` backend (the Claude Code subscription) \
                is refused for client work. Nothing was read and no model was called. Switch \
                the project's backend to the API in Settings.";
    let server = MockServer::start().await;
    stub_for_a_scan_run(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/audit/job/job-9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(job_poll(
            "failed",
            serde_json::Value::Null,
            Some(lock),
        )))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));

    let mut vdom = VirtualDom::new(results_with_a_selected_rule);
    let mut wired = Wired::default();
    wired.take(&vdom.rebuild_to_vec());
    pump_wired(&mut vdom, &mut wired, 800).await;
    wired.click_tour(&vdom, "run-inspection");
    pump_wired(&mut vdom, &mut wired, 3_000).await;

    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("Refused for a client engagement"), "{html}");
    assert!(
        html.contains("Nothing was read and no model was called"),
        "{html}"
    );
    assert!(!html.contains("Scan failed"), "{html}");
    assert!(
        !html.contains("Check your project"),
        "no backend-setup hint: {html}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_run_mode_options_say_what_is_true_of_the_engine() {
    let server = MockServer::start().await;
    // Run mode is the Engine finder's control; the first mock mounted wins.
    Mock::given(method("GET"))
        .and(path("/api/projects/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "p-1", "name": "Hearth", "repos": ["me/api"],
            "review": { "finder": "engine" },
            "backend": "api", "client_engagement": true, "client_name": "",
        })))
        .mount(&server)
        .await;
    stub_for_a_scan_run(&server).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut vdom = VirtualDom::new(results_with_a_selected_rule);
    vdom.rebuild_in_place();
    pump(&mut vdom, 800).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("Default (batch on the API backend, else parallel)"),
        "{html}"
    );
    assert!(
        html.contains("Large inspection (same as Default)"),
        "{html}"
    );
    assert!(!html.contains("Large scan"), "{html}");
    assert!(
        !html.contains("Background job (walk away, default mode)"),
        "{html}"
    );
    // One short line says every run is saved; the long text is behind the "?" button.
    assert!(
        html.contains("Every inspection is saved, so Run what remains can finish it."),
        "{html}"
    );
    assert!(html.contains("aria-label=\"About Run mode\""), "{html}");
    assert!(camerata_ui_core::help::inspection::run_mode_topic()
        .body
        .contains("the inspection is saved on the server as a job"),);
}

// ── the poll loop that every paid scan now depends on (edge-state row R11) ──────────

thread_local! {
    static POLL_SIGNALS: std::cell::RefCell<Option<PollSignals>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Copy)]
struct PollSignals {
    active: Signal<Option<String>>,
    failed: Signal<Option<String>>,
    auditing: Signal<bool>,
}

fn poll_harness() -> Element {
    crate::loading::provide_loading_context();
    let count = use_context::<crate::loading::LoadingCount>();
    let audit = use_signal(|| Option::<ScanReportView>::None);
    let auditing = use_signal(|| true);
    let job_progress = use_signal(|| Option::<(usize, usize, usize, usize)>::None);
    let det = use_signal(|| Option::<DetProgressView>::None);
    let cp = use_signal(|| None);
    let active = use_signal(|| Some("job-9".to_string()));
    let idle = use_signal(|| Option::<u128>::None);
    let failed = use_signal(|| Option::<String>::None);
    POLL_SIGNALS.with(|p| {
        *p.borrow_mut() = Some(PollSignals {
            active,
            failed,
            auditing,
        })
    });
    spawn(async move {
        poll_job(
            "job-9".to_string(),
            audit,
            auditing,
            job_progress,
            det,
            cp,
            active,
            idle,
            failed,
            count,
        )
        .await;
    });
    rsx! { div {} }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn three_failed_polls_keep_the_job_and_never_invite_a_second_paid_run() {
    // The status endpoint answers nothing usable (R11): the server may have restarted, or only
    // the connection dropped. The scan is saved and may still be running.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/audit/job/job-9"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

    let mut vdom = VirtualDom::new(poll_harness);
    vdom.rebuild_in_place();
    pump(&mut vdom, 5_500).await;

    let sig = POLL_SIGNALS.with(|p| p.borrow().expect("the harness ran"));
    let message = sig.failed.peek().clone().expect("the loss is announced");
    assert_eq!(message, LOST_CONTACT_MESSAGE);
    assert!(message.contains("may still be running"), "{message}");
    assert!(message.contains("do not start it again"), "{message}");
    assert!(
        !message.contains("retry"),
        "no invitation to run again: {message}"
    );
    assert_eq!(
        sig.active.peek().clone().as_deref(),
        Some("job-9"),
        "the job id is kept so the scan can be picked up again"
    );
    assert!(!*sig.auditing.peek());
    // The banner carries no "retry the scan" hint either.
    assert_eq!(job_failure_kind(&message), JobFailureKind::LostContact);
    assert_eq!(JobFailureKind::LostContact.hint(), None);
}

#[test]
fn the_incomplete_banner_names_what_really_stopped_the_run() {
    fn failed(reason: &str) -> Vec<FailedPassView> {
        vec![FailedPassView {
            reason: reason.to_string(),
            ..FailedPassView::default()
        }]
    }
    let limit = stop_line_for(&failed(
        "NOT RUN: the spend limit set for this run was reached",
    ))
    .unwrap();
    assert!(limit.starts_with("Stopped by the spend limit"), "{limit}");
    let unknown = stop_line_for(&failed(
        "NOT RUN: the spend limit set for this run could not be enforced (a call's cost could not be determined)",
    ))
    .unwrap();
    assert!(
        unknown.starts_with("The spend limit could not be enforced"),
        "{unknown}"
    );
    assert!(!unknown.contains("Stopped by the spend limit"), "{unknown}");
    let paused = stop_line_for(&failed(
        "NOT RUN: the run was paused because its projected cost went over the approved amount",
    ))
    .unwrap();
    assert!(
        paused.starts_with("Paused at a cost checkpoint"),
        "{paused}"
    );
    assert!(!paused.contains("spend limit"), "{paused}");
    assert_eq!(stop_line_for(&failed("overloaded")), None);
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_definite_job_unknown_answer_says_the_scan_is_gone_and_clears_the_job() {
    // A restarted server answers 200 with null for a job id it no longer holds: the job is
    // certainly dead, which is not the same as no answer.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/audit/job/job-9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::Value::Null))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut vdom = VirtualDom::new(poll_harness);
    vdom.rebuild_in_place();
    pump(&mut vdom, 2_500).await;

    let sig = POLL_SIGNALS.with(|p| p.borrow().expect("the harness ran"));
    let message = sig.failed.peek().clone().expect("the loss is announced");
    assert_eq!(message, JOB_GONE_MESSAGE);
    assert!(message.contains("no longer running"), "{message}");
    assert!(message.contains("Run what remains"), "{message}");
    assert!(!message.contains("may still be running"), "{message}");
    assert_eq!(sig.active.peek().clone(), None, "a dead job is not kept");
    assert!(!*sig.auditing.peek());
    assert_eq!(job_failure_kind(&message), JobFailureKind::Gone);
}

#[test]
fn the_live_progress_line_names_the_real_cause_or_none() {
    let paused = job_progress_label_for(3, 6, 2, 3, NotRunCause::Checkpoint);
    assert!(
        paused.contains("3 NOT RUN (paused at a cost checkpoint)"),
        "{paused}"
    );
    assert!(!paused.contains("spend limit"), "{paused}");
    let unknown = job_progress_label_for(3, 6, 2, 3, NotRunCause::Unknown);
    assert!(unknown.contains("3 NOT RUN \u{b7}"), "{unknown}");
    assert!(!unknown.contains("spend limit"), "{unknown}");
    let limit = job_progress_label_for(3, 6, 2, 3, NotRunCause::SpendLimit);
    assert!(limit.contains("3 NOT RUN (spend limit)"), "{limit}");
    let none = job_progress_label_for(3, 6, 2, 0, NotRunCause::Unknown);
    assert!(!none.contains("NOT RUN"), "{none}");
}

#[test]
fn only_a_backend_setup_failure_gets_the_backend_setup_hint() {
    for setup in [
        "This project is set to the Anthropic API backend, but no API key is configured.",
        "claude CLI (`claude`) was not found on PATH",
        "Claude Code is too old for the model claude-fable-5-2",
    ] {
        assert_eq!(job_failure_kind(setup), JobFailureKind::Failed, "{setup}");
        assert!(JobFailureKind::Failed.hint().is_some());
    }
    for other in [
        "Cannot inspect 1 of 2 repositories: acme/web (no local folder). Set the folder for each one.",
        "Unknown scan mode `turbo`.",
        "No local repos to audit. Browse to each repo's local folder first.",
    ] {
        assert_eq!(job_failure_kind(other), JobFailureKind::Other, "{other}");
    }
    assert_eq!(JobFailureKind::Other.hint(), None);
    assert_eq!(JobFailureKind::Other.heading(), "Scan failed");
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_partial_run_refusal_shows_the_servers_text_and_no_backend_setup_hint() {
    let refusal = "Cannot inspect 1 of 2 repositories: acme/web (no local folder). Set the folder \
                   for each one, or run again with allow_partial to inspect the rest.";
    let server = MockServer::start().await;
    stub_for_a_scan_run(&server).await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/audit/job/job-9"))
        .respond_with(ResponseTemplate::new(200).set_body_json(job_poll(
            "failed",
            serde_json::Value::Null,
            Some(refusal),
        )))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(results_with_a_selected_rule);
    let mut wired = Wired::default();
    wired.take(&vdom.rebuild_to_vec());
    pump_wired(&mut vdom, &mut wired, 800).await;
    wired.click_tour(&vdom, "run-inspection");
    pump_wired(&mut vdom, &mut wired, 3_000).await;

    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("Cannot inspect 1 of 2 repositories"),
        "{html}"
    );
    assert!(html.contains("Scan failed"), "{html}");
    assert!(
        !html.contains("Check your project"),
        "no setup hint for a partial run: {html}"
    );
}
