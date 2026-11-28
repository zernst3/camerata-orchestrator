//! The project's exclusion list ("Files left out by agreement") in the Inspection settings: one
//! pattern per line, saved to `POST /api/projects/:id/scan-exclusions`. The engine reads and
//! bills exactly the files the list does not name, so this field is where a quote's agreement
//! about seed data or a vendored export is recorded.

use dioxus::prelude::*;

/// The html id of the list's text box and of its Save button, so a test drives the real controls.
pub(crate) const EXCLUSIONS_TEXT_ID: &str = "scan-exclusions-text";
pub(crate) const EXCLUSIONS_SAVE_ID: &str = "scan-exclusions-save";

/// The html id of the Reload button shown when the list changed elsewhere.
pub(crate) const EXCLUSIONS_RELOAD_ID: &str = "scan-exclusions-reload";

/// Said when the list changed elsewhere and the editor holds unsaved typing.
pub(crate) const CHANGED_ELSEWHERE: &str = "The list changed elsewhere. Reload to see it.";

/// The label above the box.
pub(crate) const EXCLUSIONS_LABEL: &str = "Excluded by agreement";

/// The help text under the label: what a pattern looks like and what a listed file means.
pub(crate) const EXCLUSIONS_HELP: &str = "Files and folders you and the client agreed are not part of the inspection. One pattern per line, written as repo-name/path, for example acme-field-data/seeds/**. A folder name or a trailing slash means everything under it, a ** matches folders at any depth and a * matches within one folder. A file listed here is not read, not counted and not billed. The report lists it as excluded by agreement.";

/// The newest Excluded by agreement list the server returned, with its project: set by every
/// route that returns the project (add, remove, save), read by any open editor so a click on the
/// results screen updates it.
pub(crate) static LATEST_EXCLUSIONS: GlobalSignal<Option<(String, Vec<String>)>> =
    Signal::global(|| None);

/// Publish the list a route returned for `project_id` to every open editor.
pub(crate) fn publish_exclusions(project_id: &str, list: Vec<String>) {
    *LATEST_EXCLUSIONS.write() = Some((project_id.to_string(), list));
}

/// The patterns in `text`: one per line, trimmed, blank lines dropped.
pub(crate) fn patterns_of(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Why a save was refused: the server's words, and the list it holds now when the refusal is
/// "changed since you opened it".
pub(crate) struct SaveRefused {
    pub message: String,
    pub current: Option<Vec<String>>,
}

impl SaveRefused {
    fn plain(message: &str) -> Self {
        Self {
            message: message.to_string(),
            current: None,
        }
    }
}

/// What a successful save returns: the list the server stored (it trims and drops repeats), the
/// patterns that match no file, and the patterns that cannot be checked.
pub(crate) struct Saved {
    pub list: Vec<String>,
    pub unmatched: Vec<String>,
    pub unchecked: Vec<String>,
}

/// Save the list, or the server's reason: a pattern that is not valid is refused by name and
/// nothing is saved.
pub(crate) async fn save_scan_exclusions(
    project_id: &str,
    patterns: &[String],
    based_on: &[String],
) -> Result<Saved, SaveRefused> {
    let resp = reqwest::Client::new()
        .post(format!(
            "{}/api/projects/{}/scan-exclusions",
            crate::bff_base(),
            project_id
        ))
        .json(&serde_json::json!({ "patterns": patterns, "based_on": based_on }))
        .send()
        .await
        .map_err(|_| SaveRefused::plain("The server could not be reached."))?;
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|_| SaveRefused::plain("The server's answer could not be read."))?;
    if body.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        let list = body
            .get("project")
            .and_then(|p| p.get("scan_exclusions"))
            .and_then(|l| serde_json::from_value::<Vec<String>>(l.clone()).ok())
            .ok_or_else(|| SaveRefused::plain("The server's answer could not be read."))?;
        let strings = |key: &str| -> Vec<String> {
            body.get(key)
                .and_then(|l| serde_json::from_value(l.clone()).ok())
                .unwrap_or_default()
        };
        Ok(Saved {
            list,
            unmatched: strings("unmatched"),
            unchecked: strings("unchecked"),
        })
    } else {
        Err(SaveRefused {
            message: body
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("The list could not be saved.")
                .to_string(),
            current: body
                .get("current")
                .and_then(|l| serde_json::from_value(l.clone()).ok()),
        })
    }
}

/// The exclusion list editor. Saves on the Save button; shows the server's reason when a pattern
/// is refused.
#[component]
pub(super) fn ScanExclusionsEditor(
    project_id: String,
    initial: Vec<String>,
    /// Told the saved list after each successful save.
    #[props(default)]
    on_saved: Option<EventHandler<Vec<String>>>,
    /// Locked while a run is in flight.
    #[props(default)]
    disabled: bool,
) -> Element {
    let mut text = use_signal(|| initial.join("\n"));
    // The list the editor last loaded or saved: a save names it, and the server refuses the save
    // when the stored list has changed since, so a stale editor can never drop an agreed
    // exclusion.
    let mut base = use_signal(|| initial.clone());
    let mut unmatched = use_signal(Vec::<String>::new);
    let mut unchecked = use_signal(Vec::<String>::new);
    // A list that changed elsewhere while the editor holds unsaved typing: kept aside, never
    // written over the typing. "Reload" applies it.
    let mut pending = use_signal(|| None::<Vec<String>>);
    let mut saving = use_signal(|| false);
    let mut saved = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    // A list published elsewhere for this project re-seeds an editor with nothing unsaved. An
    // editor with unsaved typing keeps it and says the list changed. Only the published list is
    // subscribed to; the text and base are read without subscribing.
    let watched = project_id.clone();
    use_effect(move || {
        if let Some((id, list)) = LATEST_EXCLUSIONS() {
            if id == watched {
                let dirty = patterns_of(&text.peek()) != *base.peek();
                if dirty {
                    if list != *base.peek() {
                        pending.set(Some(list));
                    }
                } else {
                    text.set(list.join("\n"));
                    base.set(list);
                    pending.set(None);
                }
            }
        }
    });
    let pid = project_id.clone();
    let save = move |_| {
        let patterns = patterns_of(&text());
        let based_on = base();
        let pid = pid.clone();
        let pid_for_publish = pid.clone();
        saving.set(true);
        saved.set(false);
        error.set(None);
        spawn(async move {
            match save_scan_exclusions(&pid, &patterns, &based_on).await {
                Ok(done) => {
                    let list = done.list;
                    text.set(list.join("\n"));
                    base.set(list.clone());
                    pending.set(None);
                    publish_exclusions(&pid_for_publish, list.clone());
                    unmatched.set(done.unmatched);
                    unchecked.set(done.unchecked);
                    saved.set(true);
                    if let Some(on_saved) = on_saved {
                        on_saved.call(list);
                    }
                }
                Err(refused) => {
                    // The list the server holds now (not a possibly stale shared copy) is what
                    // "Reload" shows.
                    if let Some(current) = refused.current {
                        pending.set(Some(current));
                    }
                    error.set(Some(refused.message));
                }
            }
            saving.set(false);
        });
    };
    rsx! {
        div { class: "tier-map-editor scan-exclusions-editor",
            label {
                class: "tier-map-heading",
                r#for: "{EXCLUSIONS_TEXT_ID}",
                "{EXCLUSIONS_LABEL}"
            }
            textarea {
                id: "{EXCLUSIONS_TEXT_ID}",
                class: "scan-exclusions-text",
                rows: "4",
                spellcheck: "false",
                disabled: saving() || disabled,
                value: "{text}",
                oninput: move |e| {
                    text.set(e.value());
                    saved.set(false);
                },
            }
            span { class: "audit-model-hint", "{EXCLUSIONS_HELP}" }
            div { class: "tier-map-row",
                button {
                    id: "{EXCLUSIONS_SAVE_ID}",
                    class: "btn-edit-sm",
                    r#type: "button",
                    disabled: saving() || disabled,
                    onclick: save,
                    "Save list"
                }
                if saved() {
                    span {
                        class: "audit-model-hint inspection-settings-saved",
                        role: "status",
                        "Saved"
                    }
                }
            }
            if !unmatched().is_empty() {
                p { class: "review-settings-bad", role: "alert",
                    if unmatched().len() == 1 {
                        "This pattern matches no file, so a start is refused until it is fixed or removed: {unmatched().join(\", \")}"
                    } else {
                        "These patterns match no file, so a start is refused until they are fixed or removed: {unmatched().join(\", \")}"
                    }
                }
            }
            if !unchecked().is_empty() {
                p { class: "review-settings-bad", role: "alert",
                    if unchecked().len() == 1 {
                        "This pattern cannot be checked, so a start is refused until it can be: {unchecked().join(\", \")}"
                    } else {
                        "These patterns cannot be checked, so a start is refused until they can be: {unchecked().join(\", \")}"
                    }
                }
            }
            if let Some(list) = pending() {
                p { class: "review-settings-bad", role: "status",
                    "{CHANGED_ELSEWHERE}"
                    button {
                        id: "{EXCLUSIONS_RELOAD_ID}",
                        class: "btn-edit-sm",
                        r#type: "button",
                        onclick: move |_| {
                            text.set(list.join("\n"));
                            base.set(list.clone());
                            pending.set(None);
                            error.set(None);
                        },
                        "Reload"
                    }
                }
            }
            if let Some(why) = error() {
                p { class: "review-settings-bad", role: "alert", "{why}" }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_are_one_per_line_trimmed_with_blank_lines_dropped() {
        assert_eq!(
            patterns_of("  app/seeds/**  \n\n app/dump.sql\r\n   \n"),
            ["app/seeds/**", "app/dump.sql"]
        );
        assert!(patterns_of("").is_empty());
    }

    #[test]
    fn the_words_follow_the_house_style() {
        for text in [EXCLUSIONS_LABEL, EXCLUSIONS_HELP] {
            assert!(
                !text.contains('\u{2014}') && !text.contains('\u{2013}'),
                "{text}"
            );
            assert!(!text.contains(", and "), "no serial comma: {text}");
        }
    }

    // ── The real editor, its real Save button, a stub server ────────────────────────────────

    use dioxus::dioxus_core::{AttributeValue, ElementId, Mutation, Mutations};
    use std::time::Duration;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    thread_local! {
        static INITIAL: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn editor() -> Element {
        let initial = INITIAL.with(|i| i.borrow().clone());
        rsx! { ScanExclusionsEditor { project_id: "p1".to_string(), initial } }
    }

    #[derive(Default)]
    struct Seen {
        ids: Vec<(String, ElementId)>,
        clicks: Vec<ElementId>,
        inputs: Vec<ElementId>,
    }

    impl Seen {
        fn take(&mut self, m: &Mutations) {
            for e in &m.edits {
                match e {
                    Mutation::SetAttribute {
                        name: "id",
                        value: AttributeValue::Text(t),
                        id,
                        ..
                    } => self.ids.push((t.clone(), *id)),
                    Mutation::NewEventListener { name, id } if name == "click" => {
                        self.clicks.push(*id)
                    }
                    Mutation::NewEventListener { name, id } if name == "input" => {
                        self.inputs.push(*id)
                    }
                    _ => {}
                }
            }
        }
    }

    async fn pump(vdom: &mut VirtualDom, seen: &mut Seen, ms: u64) {
        let until = tokio::time::Instant::now() + Duration::from_millis(ms);
        while tokio::time::Instant::now() < until {
            let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
            seen.take(&vdom.render_immediate_to_vec());
        }
    }

    /// The body of the save the editor sent, waited for: the server may record other requests (a
    /// load) first, and on a busy machine the save can arrive after a fixed pump has ended.
    async fn saved_body(
        server: &MockServer,
        vdom: &mut VirtualDom,
        seen: &mut Seen,
    ) -> serde_json::Value {
        let until = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            let sent = server.received_requests().await.unwrap_or_default();
            if let Some(save) = sent.iter().find(|r| {
                r.method == wiremock::http::Method::POST
                    && r.url.path().ends_with("/scan-exclusions")
            }) {
                return serde_json::from_slice(&save.body).unwrap();
            }
            assert!(
                tokio::time::Instant::now() < until,
                "the editor never sent its save; the server saw: {:?}",
                sent.iter()
                    .map(|r| (r.method.to_string(), r.url.path().to_string()))
                    .collect::<Vec<_>>()
            );
            pump(vdom, seen, 50).await;
        }
    }

    fn press_save(vdom: &mut VirtualDom, seen: &Seen) {
        use std::any::Any;
        use std::rc::Rc;
        let save = seen
            .ids
            .iter()
            .rev()
            .find(|(t, id)| t == EXCLUSIONS_SAVE_ID && seen.clicks.contains(id))
            .map(|(_, id)| *id)
            .expect("a Save button with a click handler");
        let data: Box<dyn Any> = Box::new(dioxus::html::SerializedMouseData::default());
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime().handle_event(
            "click",
            dioxus::dioxus_core::Event::new(platform, true),
            save,
        );
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn saving_sends_the_list_and_says_saved() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p1/scan-exclusions"))
            .and(body_json(serde_json::json!({
                "patterns": ["app/seeds/**", "app/dump.sql"],
                "based_on": ["app/seeds/**", "app/dump.sql"]
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "project": { "id": "p1", "name": "P", "scan_exclusions": ["app/seeds/**", "app/dump.sql"] }
            })))
            .expect(1)
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        INITIAL.with(|i| *i.borrow_mut() = vec!["app/seeds/**".into(), "app/dump.sql".into()]);

        let mut vdom = VirtualDom::new(editor);
        let mut seen = Seen::default();
        seen.take(&vdom.rebuild_to_vec());
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains(EXCLUSIONS_LABEL), "{html}");
        assert!(
            html.contains("excluded by agreement"),
            "the help names the agreement: {html}"
        );
        press_save(&mut vdom, &seen);
        pump(&mut vdom, &mut seen, 600).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("Saved"), "{html}");
        assert!(!html.contains("role=\"alert\""), "{html}");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn an_invalid_pattern_is_refused_and_named_on_the_screen() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p1/scan-exclusions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "message": "\"app/[broken\" is not a valid pattern (unclosed character class)"
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        INITIAL.with(|i| *i.borrow_mut() = vec!["app/[broken".into()]);

        let mut vdom = VirtualDom::new(editor);
        let mut seen = Seen::default();
        seen.take(&vdom.rebuild_to_vec());
        press_save(&mut vdom, &seen);
        pump(&mut vdom, &mut seen, 600).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("app/[broken"), "the pattern is named: {html}");
        assert!(html.contains("is not a valid pattern"), "{html}");
        assert!(!html.contains(">Saved<"), "{html}");
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_save_from_a_stale_list_is_refused_with_a_reload_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p1/scan-exclusions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "message": "Excluded by agreement changed since you opened it. Reload the settings and try again."
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        INITIAL.with(|i| *i.borrow_mut() = vec!["app/dump.sql".into()]);
        let mut vdom = VirtualDom::new(editor);
        let mut seen = Seen::default();
        seen.take(&vdom.rebuild_to_vec());
        press_save(&mut vdom, &seen);
        pump(&mut vdom, &mut seen, 600).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("changed since you opened it"), "{html}");
        assert!(!html.contains(">Saved<"), "{html}");
        // What the editor sent says which list it was based on.
        let body = saved_body(&server, &mut vdom, &mut seen).await;
        assert_eq!(body["based_on"], serde_json::json!(["app/dump.sql"]));
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_saved_pattern_that_matches_no_file_is_warned_about() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p1/scan-exclusions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "project": { "id": "p1", "name": "P", "scan_exclusions": ["app/sedes/**"] },
                "unmatched": ["app/sedes/**"]
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        INITIAL.with(|i| *i.borrow_mut() = vec!["app/sedes/**".into()]);
        let mut vdom = VirtualDom::new(editor);
        let mut seen = Seen::default();
        seen.take(&vdom.rebuild_to_vec());
        press_save(&mut vdom, &seen);
        pump(&mut vdom, &mut seen, 600).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("matches no file"), "{html}");
        assert!(html.contains("app/sedes/**"), "{html}");
    }

    /// Item 8 (gap closed): a change made elsewhere (the results screen's add or remove) is
    /// published, and an open editor re-seeds from it, so its next save is based on the new list.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn an_open_editor_reseeds_from_a_list_published_elsewhere() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p1/scan-exclusions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": true,
                "project": { "id": "p1", "name": "P", "scan_exclusions": ["app/seeds/big.js"] }
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        INITIAL.with(|i| *i.borrow_mut() = Vec::new());
        let mut vdom = VirtualDom::new(editor);
        let mut seen = Seen::default();
        seen.take(&vdom.rebuild_to_vec());
        // The results screen added a file: it publishes the list the server returned.
        vdom.in_runtime(|| publish_exclusions("p1", vec!["app/seeds/big.js".to_string()]));
        pump(&mut vdom, &mut seen, 400).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("app/seeds/big.js"),
            "the open editor shows it: {html}"
        );
        // A list for another project is not shown here.
        vdom.in_runtime(|| publish_exclusions("p2", vec!["other/x".to_string()]));
        pump(&mut vdom, &mut seen, 400).await;
        assert!(!dioxus_ssr::render(&vdom).contains("other/x"));
        // Its next save is based on the new list, so it is not refused as stale.
        press_save(&mut vdom, &seen);
        pump(&mut vdom, &mut seen, 600).await;
        let body = saved_body(&server, &mut vdom, &mut seen).await;
        assert_eq!(body["based_on"], serde_json::json!(["app/seeds/big.js"]));
        assert_eq!(body["patterns"], serde_json::json!(["app/seeds/big.js"]));
    }

    fn type_text(vdom: &mut VirtualDom, seen: &Seen, text: &str) {
        use std::any::Any;
        use std::rc::Rc;
        let id = *seen
            .inputs
            .last()
            .expect("the text box has an input handler");
        let data: Box<dyn Any> = Box::new(dioxus::html::SerializedFormData::new(
            text.to_string(),
            Vec::new(),
        ));
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime()
            .handle_event("input", dioxus::dioxus_core::Event::new(platform, true), id);
    }

    fn press(vdom: &mut VirtualDom, seen: &Seen, html_id: &str) {
        use std::any::Any;
        use std::rc::Rc;
        let id = seen
            .ids
            .iter()
            .rev()
            .find(|(t, id)| t == html_id && seen.clicks.contains(id))
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no clickable {html_id}"));
        let data: Box<dyn Any> = Box::new(dioxus::html::SerializedMouseData::default());
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
        vdom.runtime()
            .handle_event("click", dioxus::dioxus_core::Event::new(platform, true), id);
    }

    /// Round 2 item 6: a list published elsewhere never overwrites unsaved typing; the editor
    /// says it changed, and Reload applies the new list.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn a_published_list_does_not_overwrite_unsaved_typing() {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        INITIAL.with(|i| *i.borrow_mut() = vec!["app/a".into()]);
        let mut vdom = VirtualDom::new(editor);
        let mut seen = Seen::default();
        seen.take(&vdom.rebuild_to_vec());
        type_text(&mut vdom, &seen, "app/typed");
        pump(&mut vdom, &mut seen, 200).await;
        vdom.in_runtime(|| publish_exclusions("p1", vec!["app/other".to_string()]));
        pump(&mut vdom, &mut seen, 400).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("app/typed"), "the typing is kept: {html}");
        assert!(!html.contains("app/other"), "{html}");
        assert!(
            html.contains("The list changed elsewhere. Reload to see it."),
            "{html}"
        );
        press(&mut vdom, &seen, EXCLUSIONS_RELOAD_ID);
        pump(&mut vdom, &mut seen, 400).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(
            html.contains("app/other") && !html.contains("app/typed"),
            "{html}"
        );
        assert!(!html.contains("changed elsewhere"), "{html}");
    }

    /// Round 2 item 6: after a stale-save refusal, Reload shows the list the refusal carried
    /// (`current`), not a stale shared copy.
    #[tokio::test(flavor = "current_thread")]
    #[serial_test::serial(process_env)]
    async fn reload_after_a_refusal_shows_the_refusals_current_list() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/projects/p1/scan-exclusions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ok": false,
                "message": "Excluded by agreement changed since you opened it. Reload the settings and try again.",
                "current": ["app/current"]
            })))
            .mount(&server)
            .await;
        let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        INITIAL.with(|i| *i.borrow_mut() = vec!["app/a".into()]);
        let mut vdom = VirtualDom::new(editor);
        let mut seen = Seen::default();
        seen.take(&vdom.rebuild_to_vec());
        // A stale shared copy from earlier is published for this project: the editor is clean
        // and takes it; then the server refuses the next save.
        vdom.in_runtime(|| publish_exclusions("p1", vec!["app/stale".to_string()]));
        pump(&mut vdom, &mut seen, 300).await;
        type_text(&mut vdom, &seen, "app/mine");
        press_save(&mut vdom, &seen);
        pump(&mut vdom, &mut seen, 600).await;
        press(&mut vdom, &seen, EXCLUSIONS_RELOAD_ID);
        pump(&mut vdom, &mut seen, 400).await;
        let html = dioxus_ssr::render(&vdom);
        assert!(html.contains("app/current"), "{html}");
        assert!(
            !html.contains("app/stale") && !html.contains("app/mine"),
            "{html}"
        );
    }

    #[test]
    fn several_listed_patterns_are_worded_in_the_plural() {
        // The wording lives in the component; its two branches are pinned here by their text.
        let src = include_str!("scan_exclusions.rs");
        assert!(src.contains("These patterns match no file"));
        assert!(src.contains("This pattern matches no file"));
    }
}
