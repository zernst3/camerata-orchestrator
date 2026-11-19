//! "Continue where you left off" on the Projects screen (Hearth-sample bug, 2026-11-19): the
//! real Open button, the real prompt, the real Continue button, then the real onboarding screen
//! it lands on. The server answers the draft read the way it now does for a draft that had no
//! review but a saved finished inspection of the same repos (the server's own end-to-end test
//! `continue_restores_the_review_from_the_saved_inspection_e2e` pins that half), so the screen
//! must show the review that came back, not a blank start.

use super::*;
use dioxus::dioxus_core::{ElementId, Mutation, Mutations};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

thread_local! {
    static TOASTS: std::cell::RefCell<Option<Signal<Vec<crate::toast::Toast>>>> =
        const { std::cell::RefCell::new(None) };
}

fn gate() -> Element {
    let toasts = use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    TOASTS.with(|t| *t.borrow_mut() = Some(toasts));
    let screen = use_context_provider(|| Signal::new(CockpitScreen::Projects));
    use_context_provider(|| Signal::new(0_usize) as crate::loading::LoadingCount);
    use_context_provider(|| Signal::new(None::<ScanReportView>));
    use_context_provider(|| Signal::new(CockpitView::Stories));
    use_context_provider(|| Signal::new(None::<String>));
    match screen() {
        CockpitScreen::Projects => rsx! { ProjectsHome {} },
        CockpitScreen::InProject => rsx! { OnboardView { connection: None } },
    }
}

/// The words on screen: markup and style blocks removed, whitespace collapsed.
fn visible_text(html: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    let mut rest = html;
    while let Some(i) = rest.find("<style>") {
        let (head, tail) = rest.split_at(i);
        for c in head.chars() {
            match c {
                '<' => in_tag = true,
                '>' => {
                    in_tag = false;
                    out.push(' ');
                }
                c if !in_tag => out.push(c),
                _ => {}
            }
        }
        rest = tail.split_once("</style>").map_or("", |(_, r)| r);
    }
    for c in rest.chars() {
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn toast_messages() -> Vec<String> {
    TOASTS.with(|t| {
        t.borrow()
            .as_ref()
            .map(|s| s.read().iter().map(|t| t.message.clone()).collect())
            .unwrap_or_default()
    })
}

#[derive(Default)]
struct Wired {
    listeners: Vec<(String, ElementId)>,
    marks: Vec<(String, ElementId)>,
}

impl Wired {
    fn take(&mut self, m: &Mutations) {
        for e in &m.edits {
            match e {
                Mutation::NewEventListener { name, id } => self.listeners.push((name.clone(), *id)),
                Mutation::SetAttribute {
                    name: "data-test",
                    value: dioxus::dioxus_core::AttributeValue::Text(text),
                    id,
                    ..
                } => self.marks.push((text.clone(), *id)),
                _ => {}
            }
        }
    }

    fn click(&self, vdom: &VirtualDom, mark: &str) {
        let id = self
            .marks
            .iter()
            .rev()
            .find(|(t, _)| t == mark)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no element marked {mark}: {:?}", self.marks));
        let data: Box<dyn std::any::Any> = Box::new(dioxus::html::SerializedMouseData::default());
        let platform: std::rc::Rc<dyn std::any::Any> =
            std::rc::Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime()
            .handle_event("click", dioxus::dioxus_core::Event::new(platform, true), id);
    }
}

async fn pump(vdom: &mut VirtualDom, w: &mut Wired, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        w.take(&vdom.render_immediate_to_vec());
    }
}

fn scan_with(findings: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "repos": ["me/api"], "files_scanned": 1, "findings": findings,
        "proposed_rules": [], "gated": false
    })
}

async fn server_with_draft(draft: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": "p-1", "name": "Hearth", "repos": ["me/api"] }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(draft)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "p-1", "name": "Hearth", "client_name": ""
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p-1/decisions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "decisions": [], "standings": [], "migrated_from_draft": true, "latest_run_id": "insp-1"
        })))
        .mount(&server)
        .await;
    server
}

async fn open_the_project(vdom: &mut VirtualDom, w: &mut Wired) {
    w.take(&vdom.rebuild_to_vec());
    pump(vdom, w, 600).await;
    w.click(vdom, "open-project");
    pump(vdom, w, 600).await;
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn continue_on_a_draft_that_got_its_review_back_lands_on_the_restored_review() {
    let findings = serde_json::json!([
        { "repo": "me/api", "path": "src/hearth_leak.rs", "line": 9, "rule_id": "R-B",
          "severity": "critical", "snippet": "leak", "detail": "d", "status": "active",
          "kind": "defect" },
    ]);
    // The draft the server hands back: scan, plus the review adopted from the saved inspection.
    let server = server_with_draft(ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "scan": scan_with(serde_json::json!([])),
        "audit": scan_with(findings),
    })))
    .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(gate);
    let mut w = Wired::default();
    open_the_project(&mut vdom, &mut w).await;

    let prompt = dioxus_ssr::render(&vdom);
    assert!(prompt.contains("Onboarding in progress"), "{prompt}");
    assert!(prompt.contains("Continue where you left off"), "{prompt}");

    w.click(&vdom, "continue-onboarding");
    pump(&mut vdom, &mut w, 1_500).await;

    let html = dioxus_ssr::render(&vdom);
    assert!(
        !html.contains("Continue where you left off"),
        "the prompt is gone: {html}"
    );
    // The restored review's own summary line, in the words the screen uses.
    let text = visible_text(&html);
    assert!(
        text.contains("1 repos 1 findings 1 high severity"),
        "the review that came back is on screen: {text}"
    );
    assert!(
        !text.contains("0 findings"),
        "it is not the blank scan: {text}"
    );
    assert!(toast_messages().is_empty(), "no error toast: {:?}", toast_messages());
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn opening_a_project_whose_draft_cannot_be_read_says_so_and_does_not_pretend_nothing_is_saved()
{
    let server = server_with_draft(ResponseTemplate::new(200).set_body_json(serde_json::json!({
        "scan": scan_with(serde_json::json!([])),
        "audit": { "repos": ["me/api"], "findings": 7 },
    })))
    .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(gate);
    let mut w = Wired::default();
    open_the_project(&mut vdom, &mut w).await;

    let messages = toast_messages();
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("The saved review in your onboarding draft could not be read")),
        "the operator is told: {messages:?}"
    );
}
