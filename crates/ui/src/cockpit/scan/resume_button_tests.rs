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
