//! Tests: the export panel and the triage log (work order E).
//!
//! The triage log left the client package. The panel has no checkbox for it; the log is an
//! operator file, downloaded from its own block, and the panel's long text is in a help dialog.

use super::AuditReportExportPanel;
use dioxus::prelude::*;

fn app() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    let dispositions = use_signal(std::collections::HashMap::new);
    rsx! {
        AuditReportExportPanel { project_id: "p1".to_string(), dispositions }
    }
}

fn html() -> String {
    let mut vdom = VirtualDom::new(app);
    vdom.rebuild_in_place();
    dioxus_ssr::render(&vdom)
}

#[test]
fn the_panel_has_no_triage_log_checkbox() {
    let html = html();
    assert!(!html.contains("export-triage-log"), "{html}");
    assert!(!html.contains(r#"type="checkbox""#), "{html}");
    assert!(
        !html.contains("Also export, per rule"),
        "the old checkbox hint is gone: {html}"
    );
    assert!(
        !html.contains(camerata_api_types::package_paths::TRIAGE_LOG_CSV),
        "{html}"
    );
}

#[test]
fn the_operator_files_block_offers_the_download_and_says_it_is_internal() {
    let html = html();
    assert!(
        html.contains("Operator files (internal, not part of the client package)"),
        "{html}"
    );
    assert!(html.contains(">Download triage log</button>"), "{html}");
}

#[test]
fn the_long_package_description_is_not_in_the_page_and_the_help_button_is() {
    let html = html();
    for old in [
        "category scorecard",
        "per-category",
        "False Positives",
        "curated findings",
        "draft mid-engagement",
        "One ZIP, three artifacts",
    ] {
        assert!(!html.contains(old), "{old} is still in the page: {html}");
    }
    assert!(
        !html.contains("The PDF is written for a board"),
        "the description is behind the help button: {html}"
    );
    assert!(
        html.contains("aria-label=\"About Export product\""),
        "{html}"
    );
    assert!(html.contains("aria-haspopup=\"dialog\""), "{html}");
    assert!(
        !html.contains("role=\"dialog\""),
        "closed until clicked: {html}"
    );
}

/// The help button opens the dialog with the package description, built from the package's own
/// names, and Close shuts it.
#[tokio::test(flavor = "current_thread")]
async fn clicking_the_help_button_opens_a_dialog_with_the_description_and_close_shuts_it() {
    use dioxus::dioxus_core::{Mutation, Mutations};
    use std::any::Any;
    use std::rc::Rc;
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let click = |vdom: &VirtualDom, id: dioxus::dioxus_core::ElementId| {
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedMouseData::default(),
        )));
        vdom.runtime()
            .handle_event("click", dioxus::dioxus_core::Event::new(platform, true), id);
    };
    let clicks = |m: &Mutations| -> Vec<dioxus::dioxus_core::ElementId> {
        m.edits
            .iter()
            .filter_map(|e| match e {
                Mutation::NewEventListener { name, id } if name == "click" => Some(*id),
                _ => None,
            })
            .collect()
    };
    fn help_only() -> Element {
        rsx! { crate::cockpit::help::HelpButton { topic: camerata_ui_core::help::export::export_topic() } }
    }
    let mut vdom = VirtualDom::new(help_only);
    let first = clicks(&vdom.rebuild_to_vec());
    // The help button is the only click listener until the dialog opens.
    click(&vdom, first[0]);
    let mut opened = clicks(&vdom.render_immediate_to_vec());
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("role=\"dialog\""), "{html}");
    assert!(html.contains("The PDF is written for a board"), "{html}");
    assert!(html.contains("Area scorecard"), "{html}");
    assert!(
        html.contains("The export is refused while any row needs a decision."),
        "{html}"
    );
    // The Close button is the one new click listener the dialog added.
    let close = opened.pop().expect("the dialog's Close button");
    click(&vdom, close);
    let _ = vdom.render_immediate_to_vec();
    let html = dioxus_ssr::render(&vdom);
    assert!(!html.contains("role=\"dialog\""), "Close shuts it: {html}");
}

/// The download button's route: `GET /api/projects/:id/triage-log.csv`, the CSV text back on
/// success and the server's message on a refusal.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_download_calls_the_triage_log_route() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/triage-log.csv"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string("INTERNAL operator file.\nRule ID\n"),
        )
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p2/triage-log.csv"))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(serde_json::json!({ "ok": false, "message": "No scan results." })),
        )
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let csv = super::fetch_triage_log("p1").await.expect("the CSV");
    assert!(csv.starts_with("INTERNAL operator file."), "{csv}");
    let refused = super::fetch_triage_log("p2").await.expect_err("a refusal");
    assert_eq!(refused, "No scan results.");
}
