//! A failed part offers the diagnostic bundle from the app (#567). These tests mount the real
//! incomplete banner, click its "Make a diagnostic bundle" button with a real click event and
//! answer the request from a stub server: the banner then shows where the bundle was saved and
//! its preview, or, when the server refused, the server's own reason.

use super::*;
use dioxus::dioxus_core::{ElementId, Mutation, Mutations};
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const BUNDLE_ROUTE: &str = "/api/inspections/run-91/diagnostic-bundle";

fn banner() -> Element {
    rsx! {
        IncompletePassesBanner {
            failed_passes: vec![FailedPassView {
                repo: "acme/api".to_string(),
                pass: "agent finder, part 2 of 3".to_string(),
                reason: "the reply could not be read".to_string(),
                class: "content".to_string(),
                ..FailedPassView::default()
            }],
            compact: false,
            run_id: Some("run-91".to_string()),
        }
    }
}

async fn pump(vdom: &mut VirtualDom, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        let _ = vdom.render_immediate_to_vec();
    }
}

/// The click listener of the element whose opening tag holds `class`, found by rendering the
/// banner once and pairing each listener with the element ids the renderer gave out in order.
fn click_on(vdom: &mut VirtualDom, mutations: &Mutations, class: &str) {
    use std::any::Any;
    use std::rc::Rc;
    let html = dioxus_ssr::render(vdom);
    assert!(html.contains(class), "no {class} in the banner: {html}");
    let ids: Vec<ElementId> = mutations
        .edits
        .iter()
        .filter_map(|e| match e {
            Mutation::NewEventListener { name, id } if name == "click" => Some(*id),
            _ => None,
        })
        .collect();
    // The banner's clickable elements, in the order they render: the failed part's copy button,
    // "Run what remains", then the bundle button. Only the bundle button sends a request.
    assert!(!ids.is_empty(), "no click listener in the banner");
    for id in ids {
        let data: Box<dyn Any> = Box::new(dioxus::html::SerializedMouseData::default());
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime()
            .handle_event("click", dioxus::dioxus_core::Event::new(platform, true), id);
    }
}

async fn bundle_posts(server: &MockServer) -> usize {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == BUNDLE_ROUTE)
        .count()
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_failed_part_offers_the_diagnostic_bundle_and_the_banner_shows_where_it_was_saved() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(BUNDLE_ROUTE))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "preview": "Camerata diagnostic bundle\nparts: 3, failed: 1",
            "json": "/Users/auditor/Library/Application Support/camerata/diagnostics/camerata-diagnostic-20261126T101500Z-ab12cd34.json",
            "preview_path": "/Users/auditor/Library/Application Support/camerata/diagnostics/camerata-diagnostic-20261126T101500Z-ab12cd34.preview.txt",
            "lookup": null,
            "notes": [],
        })))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));

    let mut vdom = VirtualDom::new(banner);
    let first = vdom.rebuild_to_vec();
    click_on(&mut vdom, &first, "scan-diagnostic-bundle");
    pump(&mut vdom, 600).await;

    assert_eq!(
        bundle_posts(&server).await,
        1,
        "one request makes the bundle"
    );
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("Diagnostic bundle saved on this computer at /Users/auditor/Library/Application Support/camerata/diagnostics/camerata-diagnostic-20261126T101500Z-ab12cd34.json"),
        "{html}"
    );
    assert!(html.contains("Nothing was sent anywhere."), "{html}");
    assert!(
        html.contains("parts: 3, failed: 1"),
        "the preview is shown: {html}"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_refused_bundle_shows_the_servers_reason() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(BUNDLE_ROUTE))
        .respond_with(ResponseTemplate::new(422).set_body_json(serde_json::json!({
            "error": "the leak check refused the bundle at /run/repos/0/name; nothing was written",
        })))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));

    let mut vdom = VirtualDom::new(banner);
    let first = vdom.rebuild_to_vec();
    click_on(&mut vdom, &first, "scan-diagnostic-bundle");
    pump(&mut vdom, 600).await;

    assert_eq!(bundle_posts(&server).await, 1);
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("The diagnostic bundle was not made: the leak check refused the bundle at /run/repos/0/name; nothing was written"),
        "{html}"
    );
    assert!(!html.contains("saved on this computer"), "{html}");
}
