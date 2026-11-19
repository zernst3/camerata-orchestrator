//! The Projects list (`ProjectsHome`): the old plain delete is now "Remove from list" and says
//! what it does not do; the server refuses it while the project still holds data, and the screen
//! shows that sentence. These tests mount the real list against a stub server and click the real
//! button.

use super::*;
use dioxus::dioxus_core::{ElementId, Mutation, Mutations};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const REFUSAL: &str = "This project still has data on this machine. Use Delete source code from this machine or Delete project and all its data first.";

thread_local! {
    static TOASTS: std::cell::RefCell<Option<Signal<Vec<crate::toast::Toast>>>> =
        const { std::cell::RefCell::new(None) };
}

fn home() -> Element {
    let toasts = use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    TOASTS.with(|t| *t.borrow_mut() = Some(toasts));
    use_context_provider(|| Signal::new(CockpitScreen::Projects));
    use_context_provider(|| Signal::new(0_usize) as crate::loading::LoadingCount);
    rsx! { ProjectsHome {} }
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
}

async fn pump(vdom: &mut VirtualDom, w: &mut Wired, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        w.take(&vdom.render_immediate_to_vec());
    }
}

fn click(vdom: &VirtualDom, id: ElementId) {
    let data: Box<dyn std::any::Any> = Box::new(dioxus::html::SerializedMouseData::default());
    let platform: std::rc::Rc<dyn std::any::Any> =
        std::rc::Rc::new(dioxus::html::PlatformEventData::new(data));
    vdom.runtime()
        .handle_event("click", dioxus::dioxus_core::Event::new(platform, true), id);
}

async fn stub(delete: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/projects"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "id": "p-1", "name": "Acme", "repos": ["acme/web"] }
        ])))
        .mount(&server)
        .await;
    Mock::given(method("DELETE"))
        .and(path("/api/projects/p-1"))
        .respond_with(delete)
        .mount(&server)
        .await;
    server
}

fn remove_button(w: &Wired) -> ElementId {
    w.marks
        .iter()
        .rev()
        .find(|(t, _)| t == "remove-from-list")
        .map(|(_, id)| *id)
        .unwrap_or_else(|| panic!("no Remove from list button: {:?}", w.marks))
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_old_delete_is_relabelled_and_sits_beside_the_data_deletes_not_in_place_of_them() {
    let server = stub(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true}))).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut vdom = VirtualDom::new(home);
    let mut w = Wired::default();
    w.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut w, 600).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("Remove from list"), "{html}");
    assert!(
        !html.contains(">Delete<"),
        "the plain button no longer says Delete: {html}"
    );
    assert!(html.contains("Delete project and all its data"), "{html}");
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn removing_a_project_that_holds_data_shows_the_servers_refusal_and_keeps_it() {
    let server = stub(ResponseTemplate::new(409).set_body_json(serde_json::json!({
        "ok": false, "code": "project_holds_data", "message": REFUSAL
    })))
    .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(home);
    let mut w = Wired::default();
    w.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut w, 600).await;

    // Two clicks: arm, then confirm.
    click(&vdom, remove_button(&w));
    pump(&mut vdom, &mut w, 200).await;
    click(&vdom, remove_button(&w));
    pump(&mut vdom, &mut w, 600).await;

    let messages = toast_messages();
    assert!(messages.iter().any(|m| m == REFUSAL), "{messages:?}");
    assert!(
        !messages.iter().any(|m| m.contains("removed from the list")),
        "{messages:?}"
    );
    assert!(dioxus_ssr::render(&vdom).contains("Acme"), "the project stays listed");
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn removing_a_project_with_no_data_is_confirmed_by_the_server() {
    let server = stub(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true}))).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(home);
    let mut w = Wired::default();
    w.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut w, 600).await;
    click(&vdom, remove_button(&w));
    pump(&mut vdom, &mut w, 200).await;
    // The first click only warns, in words that do not promise a data deletion.
    let armed = toast_messages();
    assert!(
        armed.iter().any(|m| m.contains("from the list") && m.contains("does not delete its data")),
        "{armed:?}"
    );
    assert!(!armed.iter().any(|m| m.contains("permanently delete")), "{armed:?}");
    click(&vdom, remove_button(&w));
    pump(&mut vdom, &mut w, 600).await;
    assert!(
        toast_messages().iter().any(|m| m == "Project removed from the list."),
        "{:?}",
        toast_messages()
    );
}
