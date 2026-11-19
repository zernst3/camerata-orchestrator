//! "Delete project and all its data": the separate, destructive action beside the Projects list
//! entries, apart from "Delete source code from this machine" (which keeps the findings).
//!
//! The view is props-only (`DeleteProjectModalView`) so it renders in SSR tests; the container
//! (`ProjectDangerZone`) owns the signals and the two calls:
//! - `POST /api/projects/:id/delete-all-preview`
//! - `POST /api/projects/:id/delete-all`  body `{ confirm }`  (the typed project name)

use camerata_ui_core::source::confirm_enabled;
use dioxus::prelude::*;
use serde::Deserialize;

/// One line of the plan: what will be removed and how much of it.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct PlanLine {
    pub label: String,
    pub path: String,
    pub counts: String,
}

/// Something found and left alone, with the reason.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct KeptLine {
    pub what: String,
    pub why: String,
}

/// The preview's plan.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
pub struct DeleteAllPlan {
    #[serde(default)]
    pub item_view: Vec<PlanLine>,
    #[serde(default)]
    pub total_summary: String,
    #[serde(default)]
    pub kept: Vec<KeptLine>,
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// What a finished deletion hands back.
#[derive(Clone, Debug, PartialEq)]
pub struct DeleteAllDone {
    pub record_text: String,
    pub record_path: Option<String>,
    /// Set when some location could not be removed and the project was kept.
    pub partial_message: Option<String>,
}

fn project_url(project_id: &str, tail: &str) -> String {
    format!("{}/api/projects/{}/{}", crate::bff_base(), project_id, tail)
}

fn refusal_of(body: &serde_json::Value, fallback: &str) -> String {
    body.get("message")
        .and_then(|m| m.as_str())
        .unwrap_or(fallback)
        .to_string()
}

/// Run the checks and get the plan, removing nothing. `Err` carries the server's sentence.
pub async fn preview_delete_all(project_id: &str) -> Result<DeleteAllPlan, String> {
    let resp = reqwest::Client::new()
        .post(project_url(project_id, "delete-all-preview"))
        .send()
        .await
        .map_err(|_| "Could not reach the Camerata server.".to_string())?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|_| "The server returned an unreadable response.".to_string())?;
    if body.get("ok").and_then(|b| b.as_bool()) != Some(true) {
        return Err(refusal_of(&body, "The deletion could not be planned."));
    }
    body.get("plan")
        .cloned()
        .and_then(|p| serde_json::from_value(p).ok())
        .ok_or_else(|| "The server returned an unreadable response.".to_string())
}

/// Delete the project and all its data. `confirm` is the typed name; the server compares it
/// itself, so the button gate is a convenience and never the guard.
pub async fn delete_all(project_id: &str, confirm: &str) -> Result<DeleteAllDone, String> {
    let resp = reqwest::Client::new()
        .post(project_url(project_id, "delete-all"))
        .json(&serde_json::json!({ "confirm": confirm }))
        .send()
        .await
        .map_err(|_| "Could not reach the Camerata server.".to_string())?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|_| "The server returned an unreadable response.".to_string())?;
    let text = body
        .get("record_text")
        .and_then(|t| t.as_str())
        .map(str::to_string);
    let path = body
        .get("record_path")
        .and_then(|t| t.as_str())
        .map(str::to_string);
    match (body.get("code").and_then(|c| c.as_str()), text) {
        (Some("deleted"), Some(record_text)) => Ok(DeleteAllDone {
            record_text,
            record_path: path,
            partial_message: None,
        }),
        (Some("partial"), Some(record_text)) => Ok(DeleteAllDone {
            record_text,
            record_path: path,
            partial_message: Some(refusal_of(&body, "Some locations could not be removed.")),
        }),
        _ => Err(refusal_of(&body, "The deletion was refused.")),
    }
}

/// The modal: plan, what is kept, the warning, the typed project name, and the result.
#[component]
pub fn DeleteProjectModalView(
    project_name: String,
    plan: Option<DeleteAllPlan>,
    typed: String,
    busy: bool,
    refusal: Option<String>,
    result: Option<DeleteAllDone>,
    on_typed: EventHandler<String>,
    on_confirm: EventHandler<()>,
    on_cancel: EventHandler<()>,
    on_done: EventHandler<()>,
) -> Element {
    let enabled = plan.is_some() && confirm_enabled(&typed, &project_name, false, false);
    // Marks the tests find the controls by (dynamic, so they reach the mutation stream).
    let (dp_cancel, dp_confirm, dp_name) = ("dp-cancel", "dp-confirm", "dp-name");
    rsx! {
        div { class: "rule-modal-overlay",
            div { class: "rule-modal src-modal", onclick: move |e| e.stop_propagation(),
                div { class: "rule-modal-head",
                    span { class: "rule-modal-id", "Delete project {project_name} and all its data" }
                    if result.is_none() {
                        button {
                            class: "rule-modal-close",
                            disabled: busy,
                            onclick: move |_| on_cancel.call(()),
                            "\u{2715}"
                        }
                    }
                }
                if let Some(done) = result.clone() {
                    p { class: "rule-modal-detail",
                        "The project and its data were removed from this machine. Keep this record as your proof."
                    }
                    if let Some(note) = done.partial_message.clone() {
                        p { class: "src-refusal", "{note}" }
                    }
                    if let Some(path) = done.record_path.clone() {
                        p { class: "src-method", "Record saved at {path}" }
                    }
                    div { class: "src-record",
                        pre { class: "src-record-text", "{done.record_text}" }
                    }
                    div { class: "src-modal-foot",
                        button { class: "btn-run", onclick: move |_| on_done.call(()), "Done" }
                    }
                } else {
                    p { class: "rule-modal-detail",
                        "This permanently deletes the project and everything Camerata holds for it on this machine: the checkouts, the scan caches, the saved runs, the inspections with their findings, the decisions and edits, the Engine Check, the Claude Code session files recorded for it and the project itself. This cannot be undone."
                    }
                    p { class: "src-method",
                        "To remove only the code and keep the findings, use \u{201c}Delete source code from this machine\u{201d} instead."
                    }
                    match plan.clone() {
                        None => rsx! {
                            if refusal.is_none() {
                                p { class: "rule-modal-detail", "Checking what will be removed\u{2026}" }
                            }
                        },
                        Some(plan) => rsx! {
                            ul { class: "src-plan",
                                for line in plan.item_view.iter() {
                                    li { key: "{line.label}{line.path}",
                                        span { class: "src-plan-label", "{line.label}" }
                                        span { class: "src-plan-path", "{line.path}" }
                                        span { class: "src-plan-counts", "{line.counts}" }
                                    }
                                }
                            }
                            p { class: "src-plan-total", "Total: {plan.total_summary}" }
                            for k in plan.kept.iter() {
                                p { key: "{k.what}", class: "src-method", "Kept: {k.what}. {k.why}." }
                            }
                            for w in plan.warnings.iter() {
                                p { key: "{w}", class: "src-warning", "{w}" }
                            }
                            label { class: "src-confirm-label",
                                "Type "
                                code { "{project_name}" }
                                " to confirm"
                            }
                            input {
                                class: "addressee-input src-confirm-input",
                                "data-test": dp_name,
                                placeholder: "{project_name}",
                                value: "{typed}",
                                disabled: busy,
                                oninput: move |e| on_typed.call(e.value()),
                            }
                        },
                    }
                    if let Some(message) = refusal.clone() {
                        p { class: "src-refusal", "{message}" }
                    }
                    div { class: "src-modal-foot",
                        button {
                            class: "btn-edit-sm",
                            "data-test": dp_cancel,
                            disabled: busy,
                            onclick: move |_| on_cancel.call(()),
                            "Cancel"
                        }
                        button {
                            class: "btn-solid-danger",
                            "data-test": dp_confirm,
                            disabled: !enabled || busy,
                            onclick: move |_| on_confirm.call(()),
                            if busy { "Deleting\u{2026}" } else { "Delete project and all its data" }
                        }
                    }
                }
            }
        }
    }
}

/// The collapsed danger zone of a project in the Projects list, and the modal it opens.
#[component]
pub fn ProjectDangerZone(
    project_id: String,
    project_name: String,
    on_changed: EventHandler<()>,
) -> Element {
    let dp_open = "dp-open";
    let mut open = use_signal(|| false);
    let mut plan = use_signal(|| Option::<DeleteAllPlan>::None);
    let mut typed = use_signal(String::new);
    let mut busy = use_signal(|| false);
    let mut refusal = use_signal(|| Option::<String>::None);
    let mut result = use_signal(|| Option::<DeleteAllDone>::None);

    let start = {
        let pid = project_id.clone();
        move |_| {
            let pid = pid.clone();
            plan.set(None);
            typed.set(String::new());
            refusal.set(None);
            result.set(None);
            open.set(true);
            spawn(async move {
                match preview_delete_all(&pid).await {
                    Ok(p) => plan.set(Some(p)),
                    Err(m) => refusal.set(Some(m)),
                }
            });
        }
    };
    let confirm = {
        let (pid, name) = (project_id.clone(), project_name.clone());
        move |()| {
            // The button is disabled until the name matches; the handler holds the same line.
            if !confirm_enabled(&typed(), &name, false, false) {
                return;
            }
            let (pid, text) = (pid.clone(), typed());
            busy.set(true);
            refusal.set(None);
            spawn(async move {
                match delete_all(&pid, &text).await {
                    Ok(done) => result.set(Some(done)),
                    Err(m) => refusal.set(Some(m)),
                }
                busy.set(false);
            });
        }
    };
    let finished = result().is_some();

    rsx! {
        details { class: "ws-danger pg-danger",
            summary { class: "ws-danger-summary", "Danger zone" }
            p { class: "ws-danger-text",
                "Removes this project and every copy of its data Camerata holds, findings included."
            }
            button { class: "btn-danger-outline", "data-test": dp_open, onclick: start, "Delete project and all its data" }
        }
        if open() {
            DeleteProjectModalView {
                project_name: project_name.clone(),
                plan: plan(),
                typed: typed(),
                busy: busy(),
                refusal: refusal(),
                result: result(),
                on_typed: move |v| typed.set(v),
                on_confirm: confirm,
                on_cancel: move |_| {
                    open.set(false);
                    if finished {
                        on_changed.call(());
                    }
                },
                on_done: move |_| {
                    open.set(false);
                    on_changed.call(());
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dioxus::dioxus_core::{ElementId, Mutation, Mutations};
    use std::time::Duration;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn plan_body() -> serde_json::Value {
        serde_json::json!({ "ok": true, "plan": {
            "project": "Acme",
            "item_view": [{ "label": "checkout", "path": "/w/acme/app", "counts": "12 files, 3.4 KB" }],
            "total_summary": "12 files, 3.4 KB", "kept": [], "warnings": [], "not_reached": [],
        }})
    }

    // ── props-only view ──────────────────────────────────────────────────────

    #[derive(Clone, PartialEq, Props)]
    struct Case {
        typed: String,
        loaded: bool,
    }

    fn render(case: Case) -> String {
        fn harness(case: Case) -> Element {
            rsx! {
                DeleteProjectModalView {
                    project_name: "Acme".to_string(),
                    plan: case.loaded.then(|| DeleteAllPlan {
                        item_view: vec![PlanLine {
                            label: "checkout".into(),
                            path: "/w".into(),
                            counts: "1 files, 1 B".into(),
                        }],
                        total_summary: "1 files, 1 B".into(),
                        ..DeleteAllPlan::default()
                    }),
                    typed: case.typed.clone(),
                    busy: false,
                    refusal: None,
                    result: None,
                    on_typed: move |_| {},
                    on_confirm: move |_| {},
                    on_cancel: move |_| {},
                    on_done: move |_| {},
                }
            }
        }
        let mut vdom = VirtualDom::new_with_props(harness, case);
        vdom.rebuild_in_place();
        dioxus_ssr::render(&vdom)
    }

    fn confirm_tag(html: &str) -> String {
        let at = html
            .find("btn-solid-danger")
            .expect("the confirm button renders");
        let start = html[..at].rfind("<button").unwrap();
        let end = html[at..].find('>').unwrap() + at;
        html[start..=end].to_string()
    }

    #[test]
    fn the_dialog_names_what_goes_says_it_cannot_be_undone_and_points_at_the_source_only_delete() {
        let html = render(Case {
            typed: String::new(),
            loaded: true,
        });
        assert!(
            html.contains("Delete project Acme and all its data"),
            "{html}"
        );
        assert!(html.contains("This cannot be undone."), "{html}");
        assert!(html.contains("the saved runs"), "{html}");
        assert!(
            html.contains("the inspections with their findings"),
            "{html}"
        );
        assert!(html.contains("keep the findings"), "{html}");
        assert!(
            html.contains("Delete source code from this machine"),
            "{html}"
        );
        assert!(!html.contains('\u{2014}') && !html.contains('\u{2013}'));
        assert!(!html.contains(", and"), "no serial comma");
    }

    #[test]
    fn the_confirm_button_stays_disabled_until_the_exact_project_name_is_typed() {
        for typed in ["", "acme", "Acme ", " Acme", "Acme2", "ACME"] {
            let tag = confirm_tag(&render(Case {
                typed: typed.to_string(),
                loaded: true,
            }));
            assert!(tag.contains("disabled"), "typed {typed:?}: {tag}");
        }
        let tag = confirm_tag(&render(Case {
            typed: "Acme".to_string(),
            loaded: true,
        }));
        assert!(!tag.contains("disabled"), "{tag}");
        // Nothing loaded yet: nothing can be confirmed even with the right name.
        let tag = confirm_tag(&render(Case {
            typed: "Acme".to_string(),
            loaded: false,
        }));
        assert!(tag.contains("disabled"), "{tag}");
    }

    // ── the real component against a stub server ─────────────────────────────

    #[derive(Default)]
    struct Wired {
        listeners: Vec<(String, ElementId)>,
        tests: Vec<(String, ElementId)>,
    }

    impl Wired {
        fn take(&mut self, m: &Mutations) {
            for e in &m.edits {
                match e {
                    Mutation::NewEventListener { name, id } => {
                        self.listeners.push((name.clone(), *id));
                    }
                    Mutation::SetAttribute {
                        name: "data-test",
                        value: dioxus::dioxus_core::AttributeValue::Text(text),
                        id,
                        ..
                    } => self.tests.push((text.clone(), *id)),
                    _ => {}
                }
            }
        }
    }

    async fn pump(vdom: &mut VirtualDom, wired: &mut Wired, ms: u64) {
        let until = tokio::time::Instant::now() + Duration::from_millis(ms);
        while tokio::time::Instant::now() < until {
            let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
            wired.take(&vdom.render_immediate_to_vec());
        }
    }

    fn fire(vdom: &VirtualDom, name: &str, id: ElementId) {
        use std::any::Any;
        use std::rc::Rc;
        let data: Box<dyn Any> = Box::new(dioxus::html::SerializedMouseData::default());
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime()
            .handle_event(name, dioxus::dioxus_core::Event::new(platform, true), id);
    }

    fn fire_input(vdom: &VirtualDom, id: ElementId, value: &str) {
        use std::any::Any;
        use std::rc::Rc;
        let data: Box<dyn Any> = Box::new(dioxus::html::SerializedFormData::new(
            value.to_string(),
            Default::default(),
        ));
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime()
            .handle_event("input", dioxus::dioxus_core::Event::new(platform, true), id);
    }

    fn zone() -> Element {
        rsx! {
            ProjectDangerZone {
                project_id: "p1".to_string(),
                project_name: "Acme".to_string(),
                on_changed: move |_| {},
            }
        }
    }

    /// The element currently marked `mark`.
    fn marked(w: &Wired, mark: &str) -> ElementId {
        w.tests
            .iter()
            .rev()
            .find(|(t, _)| t == mark)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no element marked {mark}: {:?}", w.tests))
    }

    async fn stub() -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p1/delete-all-preview"))
            .respond_with(ResponseTemplate::new(200).set_body_json(plan_body()))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p1/delete-all"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true, "code": "deleted", "record_text": "Camerata project deletion record",
                "record_path": "/d/deletion-records/x.json"
            })))
            .mount(&server)
            .await;
        server
    }

    async fn delete_calls(server: &MockServer) -> usize {
        server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == "/api/projects/p1/delete-all")
            .count()
    }

    #[test]
    fn the_button_is_its_own_control_and_not_the_source_only_one() {
        let mut vdom = VirtualDom::new(zone);
        vdom.rebuild_in_place();
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Delete project and all its data"), "{html}");
        assert!(
            !html.contains("Delete source code from this machine"),
            "{html}"
        );
        assert!(html.contains("Danger zone"));
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn clicking_opens_the_dialog_and_cancel_deletes_nothing() {
        let server = stub().await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let mut vdom = VirtualDom::new(zone);
        let mut w = Wired::default();
        w.take(&vdom.rebuild_to_vec());

        fire(&vdom, "click", marked(&w, "dp-open"));
        pump(&mut vdom, &mut w, 500).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Type "), "{html}");
        assert!(html.contains("/w/acme/app"), "the plan is listed: {html}");

        fire(&vdom, "click", marked(&w, "dp-cancel"));
        pump(&mut vdom, &mut w, 200).await;
        assert!(
            !dioxus_ssr::render(&vdom).contains("Type "),
            "the dialog closed"
        );
        assert_eq!(delete_calls(&server).await, 0, "cancel deletes nothing");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn typing_the_wrong_name_sends_nothing_and_the_right_name_deletes() {
        let server = stub().await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let mut vdom = VirtualDom::new(zone);
        let mut w = Wired::default();
        w.take(&vdom.rebuild_to_vec());
        fire(&vdom, "click", marked(&w, "dp-open"));
        pump(&mut vdom, &mut w, 500).await;

        let input = marked(&w, "dp-name");
        let confirm = marked(&w, "dp-confirm");

        // A wrong name: the button is disabled, and a click on it sends nothing.
        fire_input(&vdom, input, "acme");
        pump(&mut vdom, &mut w, 200).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(confirm_tag(&html).contains("disabled"), "{html}");
        fire(&vdom, "click", confirm);
        pump(&mut vdom, &mut w, 300).await;
        assert_eq!(delete_calls(&server).await, 0);

        // The exact name enables it and the click sends the typed name.
        fire_input(&vdom, input, "Acme");
        pump(&mut vdom, &mut w, 200).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(!confirm_tag(&html).contains("disabled"), "{html}");
        fire(&vdom, "click", confirm);
        pump(&mut vdom, &mut w, 500).await;
        assert_eq!(delete_calls(&server).await, 1);
        let sent: serde_json::Value = serde_json::from_slice(
            &server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .find(|r| r.url.path() == "/api/projects/p1/delete-all")
                .unwrap()
                .body,
        )
        .unwrap();
        assert_eq!(sent["confirm"], "Acme");
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Camerata project deletion record"), "{html}");
        assert!(html.contains("/d/deletion-records/x.json"), "{html}");
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn a_refusal_shows_the_servers_sentence() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p1/delete-all-preview"))
            .respond_with(ResponseTemplate::new(409).set_body_json(serde_json::json!({
                "ok": false, "code": "scan_in_progress",
                "message": "A scan is running for this project. Wait for it to finish or cancel it first."
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        let err = preview_delete_all("p1").await.expect_err("refused");
        assert!(err.contains("A scan is running"), "{err}");
    }
}
