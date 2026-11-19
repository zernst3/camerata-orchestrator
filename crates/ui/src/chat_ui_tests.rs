//! Regression tests for the chat assistant bubble, driven the way an operator drives it: the REAL
//! `ChatBubble` is mounted in a VirtualDom against a stub server, and the operator's real actions
//! (click the chat button, pick a model in the select, type, press Enter, click Send) are
//! dispatched as events. What is asserted is what the operator sees (rendered text, disabled
//! Send) and what the stub server received (the saved model, the chat request).
//!
//! Standard: docs of the UI bug regression test standard (Zach, 2026-11-18).

use std::any::Any;
use std::collections::BTreeSet;
use std::rc::Rc;
use std::time::Duration;

use dioxus::dioxus_core::{ElementId, Mutation, Mutations};
use dioxus::html::{Code, Key, Location, Modifiers, SerializedFormData, SerializedKeyboardData};
use dioxus::prelude::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::chat::{ChatBubble, FindingContext};

// ── the harness ───────────────────────────────────────────────────────────────

#[derive(Clone, PartialEq, Props)]
struct HarnessProps {
    project_open: bool,
    finding: Option<FindingContext>,
    /// Seed an issue pull for this project id into the app-lifetime pull cache.
    pulled_for: Option<String>,
}

thread_local! {
    /// The harness's "is a project open on screen" signal, so a test can open or leave a project
    /// the way the cockpit does (the real bubble re-renders with the new `project_open`).
    static OPEN_SIG: std::cell::Cell<Option<Signal<bool>>> = const { std::cell::Cell::new(None) };
}

#[component]
fn Harness(props: HarnessProps) -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    // The same app-scope rules catalog fetch main.rs provides.
    let rules = use_resource(crate::chat::fetch_rules_catalog);
    use_context_provider(|| rules);
    crate::provider_safety::provide_provider_policy_context();
    let pulled_for = props.pulled_for.clone();
    use_hook(move || {
        if let Some(id) = pulled_for {
            crate::cockpit::seed_pulled_issue_for_chat_tests(&id, 7, "client issue");
        }
    });
    let open_sig = use_signal(|| props.project_open);
    OPEN_SIG.with(|c| c.set(Some(open_sig)));
    rsx! {
        ChatBubble { project_open: open_sig(), finding: props.finding.clone() }
    }
}

/// The listeners the VirtualDom has registered, with the dynamic attributes that let a test tell
/// elements apart (the Send button is the one with a dynamic `title`; the see-more toggle is the
/// one with `aria-expanded`).
#[derive(Default)]
struct Seen {
    listeners: Vec<(String, ElementId)>,
    titled: BTreeSet<ElementId>,
    expandable: BTreeSet<ElementId>,
}

impl Seen {
    fn take(&mut self, m: &Mutations) {
        for e in &m.edits {
            match e {
                Mutation::NewEventListener { name, id } => {
                    self.listeners.push((name.clone(), *id));
                }
                Mutation::SetAttribute { name, id, .. } => {
                    if *name == "title" {
                        self.titled.insert(*id);
                    }
                    if *name == "aria-expanded" {
                        self.expandable.insert(*id);
                    }
                }
                _ => {}
            }
        }
    }
    fn latest(&self, name: &str) -> ElementId {
        self.listeners
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no {name} listener among {:?}", self.listeners))
    }
    fn latest_among(&self, name: &str, ids: &BTreeSet<ElementId>) -> ElementId {
        self.listeners
            .iter()
            .rev()
            .find(|(n, id)| n == name && ids.contains(id))
            .map(|(_, id)| *id)
            .unwrap_or_else(|| panic!("no matching {name} listener"))
    }
}

struct Page {
    vdom: VirtualDom,
    seen: Seen,
}

impl Page {
    /// Mount the real bubble (panel closed) against whatever `CAMERATA_BFF_URL` points at.
    async fn mount(project_open: bool, finding: Option<FindingContext>) -> Self {
        Self::mount_with(project_open, finding, None).await
    }

    async fn mount_with(
        project_open: bool,
        finding: Option<FindingContext>,
        pulled_for: Option<&str>,
    ) -> Self {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let mut vdom = VirtualDom::new_with_props(
            Harness,
            HarnessProps {
                project_open,
                finding,
                pulled_for: pulled_for.map(str::to_string),
            },
        );
        let mut seen = Seen::default();
        seen.take(&vdom.rebuild_to_vec());
        let mut page = Self { vdom, seen };
        page.settle(300).await;
        page
    }

    async fn settle(&mut self, ms: u64) {
        let until = tokio::time::Instant::now() + Duration::from_millis(ms);
        while tokio::time::Instant::now() < until {
            let _ =
                tokio::time::timeout(Duration::from_millis(20), self.vdom.wait_for_work()).await;
            self.seen.take(&self.vdom.render_immediate_to_vec());
        }
    }

    /// Settle until `needle` is in the rendered page (or give up after `max_ms`).
    async fn settle_until(&mut self, needle: &str, max_ms: u64) {
        let until = tokio::time::Instant::now() + Duration::from_millis(max_ms);
        while tokio::time::Instant::now() < until && !self.html().contains(needle) {
            self.settle(100).await;
        }
    }

    fn html(&self) -> String {
        dioxus_ssr::render(&self.vdom)
    }

    fn send(&self, name: &str, id: ElementId, data: Box<dyn Any>) {
        let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
        self.vdom
            .runtime()
            .handle_event(name, dioxus::dioxus_core::Event::new(platform, true), id);
    }

    fn click_id(&self, id: ElementId) {
        self.send(
            "click",
            id,
            Box::new(dioxus::html::SerializedMouseData::default()),
        );
    }

    /// Click the floating chat button (the first click listener: it is mounted with the page).
    async fn click_fab(&mut self) {
        let fab = self
            .seen
            .listeners
            .iter()
            .find(|(n, _)| n == "click")
            .unwrap()
            .1;
        self.click_id(fab);
        self.settle(300).await;
    }

    /// Pick `value` in the model select.
    async fn pick_model(&mut self, value: &str) {
        let select = self.seen.latest("change");
        self.send(
            "change",
            select,
            Box::new(SerializedFormData::new(value.to_string(), Vec::new())),
        );
        self.settle(300).await;
    }

    async fn type_text(&mut self, text: &str) {
        let input = self.seen.latest("input");
        self.send(
            "input",
            input,
            Box::new(SerializedFormData::new(text.to_string(), Vec::new())),
        );
        self.settle(100).await;
    }

    async fn press_enter(&mut self) {
        let area = self.seen.latest("keydown");
        let data = SerializedKeyboardData::new(
            Key::Enter,
            Code::Enter,
            Location::Standard,
            false,
            Modifiers::empty(),
            false,
        );
        self.send("keydown", area, Box::new(data));
        self.settle(500).await;
    }

    async fn click_send(&mut self) {
        let send = self.seen.latest_among("click", &self.seen.titled.clone());
        self.click_id(send);
        self.settle(500).await;
    }

    /// Open or leave a project on screen (what the cockpit does when the operator opens a
    /// project or goes back to the projects home).
    async fn set_project_open(&mut self, open: bool) {
        let sig = OPEN_SIG.with(|c| c.get()).expect("harness mounted");
        self.vdom.in_runtime(|| {
            let mut sig = sig;
            sig.set(open);
        });
        self.settle(600).await;
    }

    async fn click_see_more(&mut self) {
        let toggle = self
            .seen
            .latest_among("click", &self.seen.expandable.clone());
        self.click_id(toggle);
        self.settle(200).await;
    }
}

// ── the stub server ───────────────────────────────────────────────────────────

struct Stub {
    server: MockServer,
}

impl Stub {
    async fn start(chat_backend: &str, saved_chat_model: Option<&str>) -> Self {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/settings"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "chat_model": saved_chat_model,
                "chat_backend": chat_backend,
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/corpus-rules"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
                { "id": "R-1", "title": "t", "domain": "sec", "scope": "repo", "options": [] }
            ])))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "text": "ok", "backend": "cli" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/settings/chat-model"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({})))
            .mount(&server)
            .await;
        Self { server }
    }

    /// The model registry answers with two Claude models.
    async fn registry_ok(&self) {
        Mock::given(method("GET"))
            .and(path("/api/models/registry"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "models": [
                    { "id": "claude-opus-5", "display": "Opus", "provider": "claude" },
                    { "id": "claude-sonnet-5", "display": "Sonnet", "provider": "claude" }
                ]
            })))
            .mount(&self.server)
            .await;
    }

    /// The registry fails the first `n` requests (the embedded server is not ready), then answers.
    async fn registry_fails_first(&self, n: u64) {
        Mock::given(method("GET"))
            .and(path("/api/models/registry"))
            .respond_with(ResponseTemplate::new(503))
            .up_to_n_times(n)
            .with_priority(1)
            .mount(&self.server)
            .await;
        self.registry_ok().await;
    }

    /// The registry never answers (503 always).
    async fn registry_down(&self) {
        Mock::given(method("GET"))
            .and(path("/api/models/registry"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&self.server)
            .await;
    }

    /// The project context the server reports for its active project.
    async fn project(&self, client_engagement: bool, scan: bool) {
        let mut body = serde_json::json!({
            "ok": true, "project_id": "proj-16", "project_name": "Faktura Test",
            "client_engagement": client_engagement,
            "selected_rules_section": "Total selected: 96 rule(s)\nSEC-1\n"
        });
        if scan {
            body["scan_results_section"] = serde_json::json!("Total findings: 41\n");
        }
        Mock::given(method("GET"))
            .and(path("/api/projects/active/context"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&self.server)
            .await;
    }

    /// The project context answers only after `ms` (the panel must say it is loading meanwhile).
    async fn project_delayed(&self, ms: u64) {
        Mock::given(method("GET"))
            .and(path("/api/projects/active/context"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(Duration::from_millis(ms))
                    .set_body_json(serde_json::json!({
                        "ok": true, "project_id": "proj-16", "project_name": "Faktura Test",
                        "client_engagement": false,
                        "selected_rules_section": "Total selected: 96 rule(s)\n"
                    })),
            )
            .mount(&self.server)
            .await;
    }

    /// The project context cannot be loaded.
    async fn project_failing(&self) {
        Mock::given(method("GET"))
            .and(path("/api/projects/active/context"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&self.server)
            .await;
    }

    /// The chat answers with `text` (instead of the default "ok").
    async fn chat_replies(&self, text: &str) {
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "text": text, "backend": "cli" })),
            )
            .with_priority(1)
            .mount(&self.server)
            .await;
    }

    async fn requests_to(&self, verb: &str, p: &str) -> Vec<serde_json::Value> {
        self.server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.method.as_str() == verb && r.url.path() == p)
            .map(|r| serde_json::from_slice(&r.body).unwrap_or(serde_json::Value::Null))
            .collect()
    }

    async fn chat_posts(&self) -> Vec<serde_json::Value> {
        self.requests_to("POST", "/api/chat").await
    }

    async fn asked_for(&self, p: &str) -> bool {
        !self.requests_to("GET", p).await.is_empty()
    }
}

macro_rules! bff {
    ($stub:expr) => {
        camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", $stub.server.uri())
    };
}

const NO_PROJECT_LINE: &str = "No project open: answers come from the guides and the rules catalog";
const BLOCKED_LINE: &str = "This project is a client engagement: the chat runs only on the API. Set Chat backend to API in Settings, or turn off Client engagement in this project&#39;s settings.";

// ── Part 1: the model select ──────────────────────────────────────────────────

/// FIRST OPEN. The registry was not answering when the app started. The operator opens the panel
/// and the select must list the registry's models, not the lone fallback.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn first_open_heals_the_select_after_a_failed_startup_fetch() {
    let stub = Stub::start("cli", Some("default")).await;
    stub.registry_fails_first(1).await;
    let _env = bff!(stub);
    let mut page = Page::mount(false, None).await;
    page.click_fab().await;
    page.settle_until("claude-sonnet-5", 8000).await;
    let html = page.html();
    assert!(
        html.contains("claude-opus-5") && html.contains("claude-sonnet-5"),
        "the registry's models are listed; {html}"
    );
    assert!(
        !html.contains("Loading models"),
        "the loading placeholder is gone; {html}"
    );
    assert!(
        !html.contains("value=\"default\""),
        "the word default is not an option nor the selected value; {html}"
    );
}

/// REOPENED PANEL. The registry is still down at the first open (the select says it is loading,
/// not "default"); the operator closes the panel, the server comes up, and reopening heals it.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn reopening_the_panel_heals_the_select() {
    let stub = Stub::start("cli", None).await;
    stub.registry_fails_first(2).await;
    let _env = bff!(stub);
    let mut page = Page::mount(false, None).await;
    page.click_fab().await; // open: the retry fails too
    let html = page.html();
    assert!(
        html.contains("Loading models"),
        "while down the select says it is loading; {html}"
    );
    assert!(
        !html.contains(">default<"),
        "the fallback is never labelled default; {html}"
    );
    page.click_fab().await; // close
    page.click_fab().await; // reopen: the server answers now
    page.settle_until("claude-sonnet-5", 8000).await;
    let html = page.html();
    assert!(
        html.contains("claude-opus-5") && html.contains("claude-sonnet-5"),
        "{html}"
    );
}

/// REGISTRY NOT LOADED. Choosing the fallback option (or anything) saves nothing.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn with_the_registry_down_picking_an_option_saves_nothing() {
    let stub = Stub::start("cli", None).await;
    stub.registry_down().await;
    let _env = bff!(stub);
    let mut page = Page::mount(false, None).await;
    page.click_fab().await;
    page.pick_model("").await;
    page.pick_model("default").await;
    page.pick_model("claude-opus-5").await;
    assert!(
        stub.requests_to("POST", "/api/settings/chat-model")
            .await
            .is_empty(),
        "no model is saved while the registry is not loaded"
    );
}

/// REGISTRY LOADED. The operator picks a real model: it is saved, and the placeholder word never is.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn with_the_registry_loaded_picking_a_model_saves_it_and_a_placeholder_is_not_saved() {
    let stub = Stub::start("cli", None).await;
    stub.registry_ok().await;
    let _env = bff!(stub);
    let mut page = Page::mount(false, None).await;
    page.click_fab().await;
    page.settle_until("claude-sonnet-5", 5000).await;
    page.pick_model("default").await;
    page.pick_model("").await;
    assert!(stub
        .requests_to("POST", "/api/settings/chat-model")
        .await
        .is_empty());
    page.pick_model("claude-sonnet-5").await;
    let saved = stub.requests_to("POST", "/api/settings/chat-model").await;
    assert_eq!(
        saved,
        vec![serde_json::json!({ "model": "claude-sonnet-5" })]
    );
}

// ── Part 2: the panel and the prompt ──────────────────────────────────────────

/// The rules catalog row healed after a failed first fetch (it sat on "loading" forever).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_rules_catalog_row_heals_after_a_failed_first_fetch() {
    let stub = Stub::start("cli", None).await;
    stub.registry_ok().await;
    // Added after Stub::start's catalog mock, with priority, so the first request fails.
    Mock::given(method("GET"))
        .and(path("/api/corpus-rules"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&stub.server)
        .await;
    let _env = bff!(stub);
    let mut page = Page::mount(false, None).await;
    page.click_fab().await;
    page.settle_until("Governance rules catalog (live)", 8000)
        .await;
    let html = page.html();
    assert!(html.contains("Governance rules catalog (live)"), "{html}");
    assert!(!html.contains("catalog (loading"), "{html}");
}

/// NO PROJECT OPEN, with a stale client-engagement project active on the server and the Chat
/// backend on cli: the panel says so plainly, Send is allowed, and the message goes out naming no
/// project and carrying no project layer. Both send paths (Enter and the Send button).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn no_project_open_sends_no_project_layer_and_is_not_locked() {
    let stub = Stub::start("cli", None).await;
    stub.registry_ok().await;
    stub.project(true, true).await; // the stale active project
    let _env = bff!(stub);
    let mut page = Page::mount(false, None).await;
    page.click_fab().await;
    let html = page.html();
    assert!(html.contains(NO_PROJECT_LINE), "{html}");
    assert!(
        !html.contains("Faktura Test"),
        "no stale project named; {html}"
    );
    assert!(!html.contains("client engagement"), "no lock line; {html}");
    assert!(!html.contains("chat-context-blocked"), "{html}");
    // The old misleading rows are gone, and no project row is claimed.
    for old in [
        "Scan results (none yet",
        "Selected rules (none yet)",
        "Project ruleset (none yet)",
        "Development state (no stories tracked yet)",
        "Pulled issues (none pulled yet)",
    ] {
        assert!(!html.contains(old), "{old} is gone; {html}");
    }
    // Enter path.
    page.type_text("What model is this?").await;
    page.press_enter().await;
    // Send-button path.
    page.type_text("And now?").await;
    page.click_send().await;
    let posts = stub.chat_posts().await;
    assert_eq!(posts.len(), 2, "both messages went out: {posts:?}");
    for body in &posts {
        assert!(body["project_id"].is_null(), "names no project: {body}");
        let sys = body["system"].as_str().unwrap();
        assert!(
            !sys.contains("=== LAYER 3") && !sys.contains("=== LAYER 4"),
            "no project layer"
        );
        assert!(
            sys.contains("=== LAYER 2:"),
            "the rules catalog is still sent"
        );
    }
    assert!(
        !stub.asked_for("/api/projects/active/context").await
            && !stub.asked_for("/api/development/context").await,
        "no project data was even fetched"
    );
}

/// A NORMAL (non-client) PROJECT OPEN on the cli: named in the panel, rows name it and match what
/// is sent, not blocked, and the message names that project.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_normal_project_open_names_it_and_sends_its_layers() {
    let stub = Stub::start("cli", None).await;
    stub.registry_ok().await;
    stub.project(false, true).await;
    let _env = bff!(stub);
    let mut page = Page::mount(true, None).await;
    page.click_fab().await;
    page.settle_until("Project open: Faktura Test", 5000).await;
    page.click_see_more().await;
    let html = page.html();
    assert!(html.contains("Project open: Faktura Test"), "{html}");
    assert!(!html.contains("No project open"), "{html}");
    assert!(html.contains("Scan results (Faktura Test)"), "{html}");
    assert!(
        html.contains("Selected rules (96 selected, Faktura Test)"),
        "{html}"
    );
    assert!(
        !html.contains("Project ruleset"),
        "no ruleset layer, no row; {html}"
    );
    assert!(!html.contains("chat-context-blocked"), "{html}");
    page.type_text("Which rules?").await;
    page.press_enter().await;
    let posts = stub.chat_posts().await;
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0]["project_id"], "proj-16");
    let sys = posts[0]["system"].as_str().unwrap();
    assert!(sys.contains("=== LAYER 3c:") && sys.contains("Total findings: 41"));
    assert!(sys.contains("=== LAYER 3d:") && sys.contains("Total selected: 96"));
}

/// A PROJECT WITH ONLY AN ONBOARDING DRAFT (selected rules, no scan): only that layer is a row.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_project_with_only_a_draft_lists_only_the_selected_rules() {
    let stub = Stub::start("cli", None).await;
    stub.registry_ok().await;
    stub.project(false, false).await;
    let _env = bff!(stub);
    let mut page = Page::mount(true, None).await;
    page.click_fab().await;
    page.settle_until("Project open: Faktura Test", 5000).await;
    page.click_see_more().await;
    let html = page.html();
    assert!(
        html.contains("Selected rules (96 selected, Faktura Test)"),
        "{html}"
    );
    assert!(
        !html.contains("Scan results"),
        "no scan section is sent, so no row; {html}"
    );
}

/// A FOCUSED FINDING on an open project is a row and is in the message.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_focused_finding_is_a_row_and_is_sent() {
    let stub = Stub::start("cli", None).await;
    stub.registry_ok().await;
    stub.project(false, true).await;
    let _env = bff!(stub);
    let finding = FindingContext {
        rule_id: "SEC-NO-HARDCODED-SECRETS-1".into(),
        severity: "high".into(),
        path: "src/main.rs".into(),
        line: 42,
        snippet: String::new(),
        detail: "Hardcoded password literal.".into(),
        repo: "me/api".into(),
        project_id: "proj-16".into(),
    };
    let mut page = Page::mount(true, Some(finding)).await;
    page.settle(500).await; // a finding opens the panel by itself
    page.settle_until("Project open: Faktura Test", 5000).await;
    page.click_see_more().await;
    let html = page.html();
    assert!(
        html.contains("Focused finding (SEC-NO-HARDCODED-SECRETS-1 at src/main.rs:42)"),
        "{html}"
    );
    page.type_text("Why?").await;
    page.press_enter().await;
    let posts = stub.chat_posts().await;
    assert!(posts[0]["system"]
        .as_str()
        .unwrap()
        .contains("=== LAYER 4:"));
}

// ── Part 3: the client lock follows the open project ─────────────────────────

/// A CLIENT ENGAGEMENT PROJECT OPEN on the cli: the one-line explanation is shown before the
/// operator types, Send is disabled with the reason, and neither Enter nor the Send button
/// sends anything.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_project_on_the_cli_says_so_and_cannot_send() {
    let stub = Stub::start("cli", None).await;
    stub.registry_ok().await;
    stub.project(true, true).await;
    let _env = bff!(stub);
    let mut page = Page::mount(true, None).await;
    page.click_fab().await;
    page.settle_until("chat-context-blocked", 5000).await;
    let html = page.html();
    assert!(html.contains("Project open: Faktura Test"), "{html}");
    assert!(html.contains(BLOCKED_LINE), "{html}");
    page.type_text("Hello").await;
    let html = page.html();
    assert!(
        html.contains(
            "class=\"chat-send\" disabled=true title=\"This project is a client engagement"
        ),
        "Send stays disabled with the reason even with text typed; {html}"
    );
    assert!(html.contains("disabled=true"), "{html}");
    page.press_enter().await;
    page.click_send().await;
    assert!(
        stub.chat_posts().await.is_empty(),
        "nothing is sent on the cli"
    );
}

/// A CLIENT ENGAGEMENT PROJECT on the API backend is allowed: no line, and the message goes out
/// naming that project (the server then applies that project's lock).
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_client_project_on_the_api_is_allowed() {
    let stub = Stub::start("api", None).await;
    stub.registry_ok().await;
    stub.project(true, true).await;
    let _env = bff!(stub);
    let mut page = Page::mount(true, None).await;
    page.click_fab().await;
    page.settle_until("Project open: Faktura Test", 5000).await;
    assert!(!page.html().contains("chat-context-blocked"));
    page.type_text("Hello").await;
    page.press_enter().await;
    let posts = stub.chat_posts().await;
    assert_eq!(posts.len(), 1, "{posts:?}");
    assert_eq!(posts[0]["project_id"], "proj-16");
}

// ── Round 2: a project's conversation, finding and pulled issues stay with that project ──

/// LEAVING A CLIENT PROJECT. The replies about it are gone from the transcript, and the next
/// message (from the home screen) carries no history of it.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn leaving_a_client_project_clears_the_conversation() {
    let stub = Stub::start("api", None).await;
    stub.registry_ok().await;
    stub.project(true, true).await;
    stub.chat_replies("CLIENT-SECRET-REPLY").await;
    let _env = bff!(stub);
    let mut page = Page::mount(true, None).await;
    page.click_fab().await;
    page.settle_until("Project open: Faktura Test", 5000).await;
    page.type_text("Tell me about the client").await;
    page.press_enter().await;
    page.settle_until("CLIENT-SECRET-REPLY", 5000).await;
    assert!(
        page.html().contains("CLIENT-SECRET-REPLY"),
        "the reply is shown"
    );
    // Back to the projects home.
    page.set_project_open(false).await;
    let html = page.html();
    assert!(!html.contains("CLIENT-SECRET-REPLY"), "reply gone; {html}");
    assert!(
        !html.contains("Tell me about the client"),
        "question gone; {html}"
    );
    assert!(html.contains(NO_PROJECT_LINE), "{html}");
    // The next message from the home screen carries no history.
    page.type_text("Anything else?").await;
    page.press_enter().await;
    let posts = stub.chat_posts().await;
    let last = posts.last().unwrap();
    assert_eq!(
        last["history"],
        serde_json::json!([]),
        "no history rides along: {last}"
    );
    assert!(last["project_id"].is_null());
}

/// A FINDING from another project is not shown and not sent under the open project.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_finding_from_another_project_is_not_sent_under_the_open_project() {
    let stub = Stub::start("cli", None).await;
    stub.registry_ok().await;
    stub.project(false, true).await; // proj-16, a normal project
    let _env = bff!(stub);
    let finding = FindingContext {
        rule_id: "SEC-NO-HARDCODED-SECRETS-1".into(),
        severity: "high".into(),
        path: "src/main.rs".into(),
        line: 42,
        snippet: "client secret snippet".into(),
        detail: "from the client project".into(),
        repo: "client/api".into(),
        project_id: "proj-CLIENT".into(),
    };
    let mut page = Page::mount(true, Some(finding)).await; // a finding opens the panel itself
    page.settle(500).await;
    page.settle_until("Project open: Faktura Test", 5000).await;
    page.click_see_more().await;
    assert!(!page.html().contains("Focused finding"), "{}", page.html());
    page.type_text("Why?").await;
    page.press_enter().await;
    let posts = stub.chat_posts().await;
    let sys = posts[0]["system"].as_str().unwrap();
    assert!(
        !sys.contains("=== LAYER 4:") && !sys.contains("client secret snippet"),
        "{sys}"
    );
}

/// PULLED ISSUES belong to the project they were pulled for. Another project's pull is not shown
/// and not sent; the open project's own pull is both.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn pulled_issues_follow_the_project_they_were_pulled_for() {
    for (pulled_for, expect_rows) in [("proj-CLIENT", false), ("proj-16", true)] {
        let stub = Stub::start("cli", None).await;
        stub.registry_ok().await;
        stub.project(false, true).await;
        let _env = bff!(stub);
        let mut page = Page::mount_with(true, None, Some(pulled_for)).await;
        page.click_fab().await;
        page.settle_until("Project open: Faktura Test", 5000).await;
        page.click_see_more().await;
        let html = page.html();
        assert_eq!(
            html.contains("Pulled issues"),
            expect_rows,
            "{pulled_for}; {html}"
        );
        page.type_text("Which issues?").await;
        page.press_enter().await;
        let posts = stub.chat_posts().await;
        let sys = posts[0]["system"].as_str().unwrap();
        assert_eq!(sys.contains("=== LAYER 3b:"), expect_rows, "{pulled_for}");
        assert_eq!(sys.contains("client issue"), expect_rows, "{pulled_for}");
    }
}

/// A project is open but its context is still loading, or failed: the panel never says "No
/// project open" (that is the home screen), and the two states read differently.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn an_open_project_whose_context_is_loading_or_failed_is_not_called_no_project() {
    // Still loading.
    let stub = Stub::start("cli", None).await;
    stub.registry_ok().await;
    stub.project_delayed(1200).await;
    let _env = bff!(stub);
    let mut page = Page::mount(true, None).await;
    page.click_fab().await;
    let html = page.html();
    assert!(
        html.contains("Loading this project&#39;s context"),
        "{html}"
    );
    assert!(!html.contains("No project open"), "{html}");
    page.settle_until("Project open: Faktura Test", 8000).await;
    assert!(page.html().contains("Project open: Faktura Test"));
    drop(_env);
    // Failed.
    let stub = Stub::start("cli", None).await;
    stub.registry_ok().await;
    stub.project_failing().await;
    let _env = bff!(stub);
    let mut page = Page::mount(true, None).await;
    page.click_fab().await;
    page.settle_until("Could not load this project", 5000).await;
    let html = page.html();
    assert!(
        html.contains("Could not load this project&#39;s context"),
        "{html}"
    );
    assert!(!html.contains("No project open"), "{html}");
    assert!(!html.contains("Loading this project"), "{html}");
}
