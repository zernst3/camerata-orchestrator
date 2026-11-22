//! Real-click tests for the convention model's controls (design 2026-11-20, section 11, tests 22
//! to 26). The real results screen (or the real Rule alternatives panel) is mounted over a stub
//! server, the controls are clicked the way a reviewer clicks them, and the assertions read what
//! went to the server and what the screen then shows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use camerata_api_types::audit::{
    CensusPlace, FinderRowRef, OptionBasis, OptionCensus, PlaceStatus, RuleConvention,
    StandardSource, StandardState,
};
use dioxus::dioxus_core::{AttributeValue, ElementId, Mutation, Mutations, ScopeId};
use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use super::*;

const SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";

// ── What the mounted screen has told us about its elements ───────────────────────────────────

struct Mounted {
    vdom: VirtualDom,
    listeners: Rc<RefCell<Vec<(String, ElementId)>>>,
    marks: Rc<RefCell<HashMap<ElementId, String>>>,
}

impl Mounted {
    fn mount(app: fn() -> Element) -> Self {
        dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
        let mut vdom = VirtualDom::new(app);
        let this = Self {
            vdom: VirtualDom::new(|| rsx! {}),
            listeners: Rc::default(),
            marks: Rc::default(),
        };
        this.take(&vdom.rebuild_to_vec());
        Self { vdom, ..this }
    }

    fn take(&self, m: &Mutations) {
        for edit in &m.edits {
            match edit {
                Mutation::SetAttribute {
                    name,
                    value: AttributeValue::Text(text),
                    id,
                    ..
                } if matches!(
                    *name,
                    "data-control" | "data-place" | "data-rule" | "data-kind" | "id"
                ) =>
                {
                    self.marks
                        .borrow_mut()
                        .insert(*id, format!("{name}={text}"));
                }
                Mutation::NewEventListener { name, id } => {
                    self.listeners.borrow_mut().push((name.clone(), *id));
                }
                _ => {}
            }
        }
    }

    /// Forget every element seen so far, so what is found next was drawn after this call.
    fn forget(&self) {
        self.listeners.borrow_mut().clear();
        self.marks.borrow_mut().clear();
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

    /// Pump until the page satisfies `ready`, or fail with the page.
    async fn until(&mut self, what: &str, ready: impl Fn(&str) -> bool) -> String {
        let mut html = String::new();
        for _ in 0..80 {
            self.pump(50).await;
            html = self.html();
            if ready(&html) {
                return html;
            }
        }
        panic!("never reached: {what}\n{html}");
    }

    fn html(&self) -> String {
        dioxus_ssr::render(&self.vdom)
    }

    /// The elements with a listener for `event` that carry the mark (`name=value`), in the order
    /// they were drawn.
    fn with_mark(&self, event: &str, mark: &str) -> Vec<ElementId> {
        let marks = self.marks.borrow();
        let mut out: Vec<ElementId> = Vec::new();
        for (name, id) in self.listeners.borrow().iter() {
            if name == event && marks.get(id).is_some_and(|m| m == mark) && !out.contains(id) {
                out.push(*id);
            }
        }
        out
    }

    /// The elements drawn since the last [`Self::forget`] that carry a mark starting `prefix`,
    /// with the full mark, in the order they were drawn.
    fn marked(&self, prefix: &str) -> Vec<String> {
        let marks = self.marks.borrow();
        let mut ids: Vec<&ElementId> = marks.keys().collect();
        ids.sort_by_key(|id| id.0);
        ids.into_iter()
            .map(|id| marks[id].clone())
            .filter(|m| m.starts_with(prefix))
            .collect()
    }

    fn click(&self, mark: &str) {
        let id = *self
            .with_mark("click", mark)
            .first()
            .unwrap_or_else(|| panic!("no clickable {mark}: {:?}", self.marks.borrow()));
        let data: Rc<dyn std::any::Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedMouseData::default(),
        )));
        self.vdom
            .runtime()
            .handle_event("click", dioxus::dioxus_core::Event::new(data, true), id);
    }

    fn pick(&self, mark: &str, value: &str) {
        let id = *self
            .with_mark("change", mark)
            .first()
            .unwrap_or_else(|| panic!("no select {mark}: {:?}", self.marks.borrow()));
        let data: Rc<dyn std::any::Any> = Rc::new(dioxus::html::PlatformEventData::new(Box::new(
            dioxus::html::SerializedFormData::new(value.to_string(), Vec::new()),
        )));
        self.vdom
            .runtime()
            .handle_event("change", dioxus::dioxus_core::Event::new(data, true), id);
    }

    /// Press Escape on the element with this html id.
    fn escape(&self, html_id: &str) {
        use dioxus::html::{Code, Key, Location, Modifiers, SerializedKeyboardData};
        let id = self.with_mark("keydown", &format!("id={html_id}"));
        let id = *id
            .last()
            .unwrap_or_else(|| panic!("no key handler on #{html_id}"));
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
            id,
        );
    }

    /// A context the screen provided, found by asking the scopes it created.
    fn context<T: Clone + 'static>(&self) -> T {
        (ScopeId::APP.0..ScopeId::APP.0 + 60)
            .find_map(|id| {
                self.vdom
                    .in_scope(ScopeId(id), || dioxus::prelude::try_consume_context::<T>())
            })
            .expect("the screen provides this context")
    }
}

// ── The fixture: the Hearth shape of design test 1, as a convention record ───────────────────

fn record(status_of_default_place: PlaceStatus) -> RuleConvention {
    let place = |path: &str, line: usize, status: PlaceStatus| CensusPlace {
        path: path.into(),
        line,
        text: format!("code at {path}"),
        checked: true,
        status,
    };
    RuleConvention {
        repo: "me/api".into(),
        rule_id: "ARCH-PAR-1".into(),
        default_option_id: Some("par".into()),
        judged_option_id: Some("par".into()),
        observed_option_id: Some("seq".into()),
        assessed_places: Some(5),
        places_recorded: true,
        options: vec![
            OptionCensus {
                option_id: "par".into(),
                label: "Run in parallel".into(),
                count: Some(1),
                places: vec![place("src/p.ts", 3, status_of_default_place)],
                ..OptionCensus::default()
            },
            OptionCensus {
                option_id: "seq".into(),
                label: "Sequential".into(),
                count: Some(4),
                places: vec![place("src/s.ts", 7, PlaceStatus::Conforms)],
                ..OptionCensus::default()
            },
        ],
        standard: StandardState {
            option_id: Some("seq".into()),
            source: StandardSource::Observed,
            differs_from_default: Some("Run in parallel".into()),
            ..StandardState::default()
        },
        finder_rows: vec![FinderRowRef {
            path: "src/f.ts".into(),
            line: 9,
            note: "N+1 queries".into(),
            option_basis: OptionBasis::OnlyOther,
            option_id: Some("seq".into()),
            status: PlaceStatus::Conforms,
            ..FinderRowRef::default()
        }],
    }
}

fn report_json() -> Value {
    json!({
        "repos": ["me/api"], "files_scanned": 1, "findings": [], "proposed_rules": [],
        "gated": false,
        "provenance": { "audited_refs": [{ "repo": "me/api", "sha": SHA }] },
        "conventions": [serde_json::to_value(record(PlaceStatus::FollowsRuleDefault)).expect("json")],
    })
}

thread_local! {
    static REPORT: RefCell<Value> = const { RefCell::new(Value::Null) };
}

fn screen() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<ScanReportView>));
    use_context_provider(|| Signal::new(CockpitView::Stories));
    use_context_provider(|| Signal::new(None::<String>));
    use_context_provider(|| Signal::new(0_usize) as crate::loading::LoadingCount);
    let report: ScanReportView =
        serde_json::from_value(REPORT.with(|r| r.borrow().clone())).expect("the scan");
    rsx! { ScanResults { report } }
}

/// A source file for any path: line N reads "line N of <path>", 30 lines, so the line a test asks
/// for is always in it.
fn source_file(req: &Request) -> ResponseTemplate {
    let path = req
        .url
        .query_pairs()
        .find(|(k, _)| k == "path")
        .map(|(_, v)| v.to_string())
        .unwrap_or_default();
    let lines: Vec<Value> = (1..=30)
        .map(|n| json!([[null, format!("line {n} of {path}")]]))
        .collect();
    ResponseTemplate::new(200).set_body_json(json!({
        "status": "ok", "source": "git", "commit": SHA, "head_differs": false,
        "language": "typescript", "total_lines": 30,
        "window": {"from": 1, "to": 30, "total_lines": 30}, "highlight": "plain",
        "classes": [], "lines": lines
    }))
}

async fn stub_server() -> MockServer {
    stub_server_answering(ResponseTemplate::new(200).set_body_json(json!({"ok": true}))).await
}

/// The stub server, with `answer` as what a ruling gets.
async fn stub_server_answering(answer: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/projects/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "p1", "name": "Acme", "repos": ["me/api"], "client_name": ""
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/source-file"))
        .respond_with(source_file)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/standards"))
        .respond_with(answer)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/standards/std-000001/revoke"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    server
}

/// The screen over `report`, loaded, with the hub open on the rule.
async fn screen_with_the_hub_open(report: Value) -> Mounted {
    REPORT.with(|r| *r.borrow_mut() = report);
    let mut m = Mounted::mount(screen);
    // The Queue draws its pane once the screen has read the project and its decisions.
    m.pump(400).await;
    m.forget();
    let hub = m.context::<crate::cockpit::viewer::ViewerHubOpen>();
    m.vdom.in_runtime(|| {
        let mut sig = hub.0;
        sig.set(Some(RuleHubRequest {
            repo: "me/api".into(),
            rule_id: "ARCH-PAR-1".into(),
        }));
    });
    m.until("the hub shows the rule", |h| h.contains("rule-hub-modal"))
        .await;
    m
}

async fn posts(server: &MockServer, route: &str) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == route)
        .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
        .collect()
}

// ── Test 22: every place the hub lists opens the code viewer at its path and line ─────────────

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn each_place_the_hub_lists_opens_the_code_viewer_at_its_path_and_line() {
    let server = stub_server().await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut m = screen_with_the_hub_open(report_json()).await;

    // The three places of the record: one that conforms, one that follows the rule's default and
    // one the finder reported.
    let wanted = [("src/s.ts", 7), ("src/p.ts", 3), ("src/f.ts", 9)];
    let mut drawn = m.marked("data-place=");
    drawn.sort();
    let mut expected: Vec<String> = wanted
        .iter()
        .map(|(p, l)| format!("data-place={p}:{l}"))
        .collect();
    expected.sort();
    assert_eq!(
        drawn,
        expected,
        "every place is an open control: {}",
        m.html()
    );
    for (path, line) in wanted {
        m.click(&format!("data-place={path}:{line}"));
        let html = m
            .until("the viewer shows the file", |h| {
                h.contains("cv-panel") && h.contains(&format!("line {line} of {path}"))
            })
            .await;
        // The viewer is over the hub: the hub is still drawn under it.
        assert!(html.contains("rule-hub-modal"), "{html}");
        let asked: Vec<HashMap<String, String>> = server
            .received_requests()
            .await
            .unwrap_or_default()
            .iter()
            .filter(|r| r.url.path() == "/api/onboard/source-file")
            .map(|r| {
                r.url
                    .query_pairs()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect()
            })
            .collect();
        let last = asked.last().expect("the viewer asked for the file");
        assert_eq!(last.get("path").map(String::as_str), Some(path));
        assert_eq!(
            last.get("around").map(String::as_str),
            Some(line.to_string().as_str())
        );
        assert_eq!(last.get("repo").map(String::as_str), Some("me/api"));
        assert_eq!(
            last.get("sha").map(String::as_str),
            Some(SHA),
            "the inspected commit"
        );
        assert_eq!(last.get("project").map(String::as_str), Some("p1"));
        // Escape closes the viewer; the hub is as it was.
        m.escape("camerata-code-viewer");
        let html = m
            .until("the viewer closes", |h| !h.contains("cv-panel"))
            .await;
        assert!(html.contains("rule-hub-modal"), "{html}");
    }
}

// ── Tests 23 and 24: the controls post the ruling and the screen reads the review again ──────

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn accept_posts_the_ruling_and_the_screen_reads_the_review_again() {
    let server = stub_server().await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut m = screen_with_the_hub_open(report_json()).await;
    let refresh = m.context::<crate::cockpit::tree::ShellRefresh>();
    let before = m.vdom.in_runtime(|| *refresh.0.peek());

    m.click("data-control=accept");
    let html = m
        .until("the hub closes", |h| !h.contains("rule-hub-modal"))
        .await;
    let sent = posts(&server, "/api/projects/p1/standards").await;
    assert_eq!(
        sent,
        vec![json!({
            "repo": "me/api", "rule_id": "ARCH-PAR-1",
            "choice": {"accept": {"option_id": "seq"}}, "reason": ""
        })],
        "{html}"
    );
    let after = m.vdom.in_runtime(|| *refresh.0.peek());
    assert_eq!(
        after,
        before + 1,
        "the review is read again (the Queue follows it)"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn choosing_another_option_posts_that_option() {
    let server = stub_server().await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut m = screen_with_the_hub_open(report_json()).await;
    m.pick("data-control=other-option", "par");
    m.until("the hub closes", |h| !h.contains("rule-hub-modal"))
        .await;
    let sent = posts(&server, "/api/projects/p1/standards").await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["choice"], json!({"accept": {"option_id": "par"}}));
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn leaving_it_to_the_client_replaces_leave_as_reported_and_posts_the_choice() {
    let server = stub_server().await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut m = screen_with_the_hub_open(report_json()).await;
    let html = m.html();
    assert!(
        html.contains("No recommendation, leave it to the client"),
        "{html}"
    );
    assert!(
        html.contains(
            "The report lists the options and the places that follow each, with none suggested."
        ),
        "{html}"
    );
    assert!(!html.contains("Leave as reported"), "{html}");
    m.click("data-control=leave");
    m.until("the hub closes", |h| !h.contains("rule-hub-modal"))
        .await;
    let sent = posts(&server, "/api/projects/p1/standards").await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["choice"], json!("leave_to_client"));
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn withdraw_posts_the_revoke_of_the_live_ruling() {
    let server = stub_server().await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut live = record(PlaceStatus::FollowsRuleDefault);
    live.standard.source = StandardSource::Reviewer;
    live.standard.ruling_id = Some("std-000001".into());
    let mut report = report_json();
    report["conventions"] = json!([serde_json::to_value(live).expect("json")]);
    let mut m = screen_with_the_hub_open(report).await;
    m.click("data-control=withdraw");
    m.until("the hub closes", |h| !h.contains("rule-hub-modal"))
        .await;
    assert_eq!(
        posts(&server, "/api/projects/p1/standards/std-000001/revoke")
            .await
            .len(),
        1
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_refused_ruling_says_why_keeps_the_hub_open_and_does_not_refresh() {
    let server = stub_server_answering(ResponseTemplate::new(422).set_body_json(json!({
        "code": "unknown_option", "message": "That option is not one of the rule's options."
    })))
    .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut m = screen_with_the_hub_open(report_json()).await;
    let refresh = m.context::<crate::cockpit::tree::ShellRefresh>();
    let before = m.vdom.in_runtime(|| *refresh.0.peek());
    m.click("data-control=accept");
    m.pump(400).await;
    assert_eq!(posts(&server, "/api/projects/p1/standards").await.len(), 1);
    let toasts = m.context::<Signal<Vec<crate::toast::Toast>>>();
    let said: Vec<String> = m
        .vdom
        .in_runtime(|| toasts.peek().iter().map(|t| t.message.clone()).collect());
    assert!(
        said.contains(&"That option is not one of the rule's options.".to_string()),
        "the refusal is said: {said:?}"
    );
    assert!(m.html().contains("rule-hub-modal"), "the hub stays open");
    assert_eq!(
        m.vdom.in_runtime(|| *refresh.0.peek()),
        before,
        "nothing to refresh"
    );
}

// ── Test 26: Rule alternatives posts a ruling, and shows no rescan bar for such a rule ───────

fn alt_rule() -> ProposedRuleView {
    serde_json::from_value(json!({
        "id": "ARCH-PAR-1", "title": "Independent awaits", "kind": "semantic",
        "options": [
            {"id": "par", "label": "Run in parallel", "directive": "d1", "why": ""},
            {"id": "seq", "label": "Sequential", "directive": "d2", "why": ""}
        ]
    }))
    .expect("a rule with two options")
}

thread_local! {
    static WITH_RECORD: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
    /// A second repo that has its own record of the same rule.
    static SECOND_REPO: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn alternatives() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(HashMap::<String, String>::new()));
    use_context_provider(|| crate::cockpit::tree::ShellRefresh(Signal::new(0u32)));
    let audit = use_signal(|| Option::<ScanReportView>::None);
    let recommendations: HashMap<String, RuleRecommendationView> = HashMap::from([(
        "ARCH-PAR-1".to_string(),
        RuleRecommendationView {
            recommended_option_id: "par".into(),
            ..RuleRecommendationView::default()
        },
    )]);
    let mut conventions = if WITH_RECORD.with(std::cell::Cell::get) {
        vec![record(PlaceStatus::FollowsRuleDefault)]
    } else {
        Vec::new()
    };
    let mut repos = vec!["me/api".to_string()];
    if SECOND_REPO.with(std::cell::Cell::get) {
        let mut web = record(PlaceStatus::FollowsRuleDefault);
        web.repo = "me/web".into();
        conventions.push(web);
        repos.push("me/web".to_string());
    }
    rsx! {
        RuleAlternativesPanel {
            project_id: "p1".to_string(),
            proposed_rules: vec![alt_rule()],
            findings: Vec::<FindingView>::new(),
            repos,
            recommendations,
            audit,
            conventions,
        }
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn rule_alternatives_posts_a_ruling_and_shows_no_rescan_bar_for_a_rule_with_a_record() {
    let server = stub_server().await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    WITH_RECORD.with(|w| w.set(true));
    let mut m = Mounted::mount(alternatives);
    m.pump(100).await;
    let html = m.html();
    assert!(
        html.contains("Observed in the code"),
        "the badge says where it came from: {html}"
    );
    assert!(!html.contains("AI recommended"), "{html}");
    let refresh = m.context::<crate::cockpit::tree::ShellRefresh>();
    let before = m.vdom.in_runtime(|| *refresh.0.peek());
    m.pick("data-rule=ARCH-PAR-1", "par");
    m.pump(300).await;
    let sent = posts(&server, "/api/projects/p1/standards").await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["rule_id"], "ARCH-PAR-1");
    assert_eq!(sent[0]["choice"], json!({"accept": {"option_id": "par"}}));
    let html = m.html();
    assert!(
        !html.contains("rec-sticky-bar") && !html.contains("Rescan"),
        "{html}"
    );
    assert_eq!(m.vdom.in_runtime(|| *refresh.0.peek()), before + 1);
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn rule_alternatives_without_a_record_still_stages_a_rescan() {
    let server = stub_server().await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    WITH_RECORD.with(|w| w.set(false));
    let mut m = Mounted::mount(alternatives);
    m.pump(100).await;
    m.pick("data-rule=ARCH-PAR-1", "seq");
    m.pump(200).await;
    let html = m.html();
    assert!(
        html.contains("rec-sticky-bar") && html.contains("Rescan 1 overridden rule"),
        "{html}"
    );
    assert!(posts(&server, "/api/projects/p1/standards")
        .await
        .is_empty());
    WITH_RECORD.with(|w| w.set(true));
}

/// Review item 12: with two repos that both have a record of the rule, a pick rules on the repo
/// the row is about (the one scanned last), not on the first record found by rule id.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn rule_alternatives_with_two_repos_rules_on_the_repo_of_the_row() {
    let server = stub_server().await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    WITH_RECORD.with(|w| w.set(true));
    SECOND_REPO.with(|w| w.set(true));
    let mut m = Mounted::mount(alternatives);
    m.pump(100).await;
    m.pick("data-rule=ARCH-PAR-1", "par");
    m.pump(300).await;
    SECOND_REPO.with(|w| w.set(false));
    let sent = posts(&server, "/api/projects/p1/standards").await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["repo"], "me/web", "{sent:?}");
}

// ── Tests 23 and 24 on a split: the No convention panel and the export line ───────────────────

/// A rule split between two options: no standard, a proposal, two options of two places each.
fn split_record() -> RuleConvention {
    let mut c = record(PlaceStatus::NoStandard);
    c.observed_option_id = None;
    c.standard = StandardState {
        proposal: Some(camerata_api_types::audit::Proposal {
            option_id: "par".into(),
            basis: camerata_api_types::audit::ProposalBasis::RuleDefault,
        }),
        ..StandardState::default()
    };
    c.options[0].count = Some(2);
    c.options[1].count = Some(2);
    c.finder_rows = Vec::new();
    c
}

fn split_report_json() -> Value {
    json!({
        "repos": ["me/api"], "files_scanned": 1, "proposed_rules": [],
        "gated": false,
        "provenance": { "audited_refs": [{ "repo": "me/api", "sha": SHA }] },
        "findings": [{
            "repo": "me/api", "path": "", "line": 0, "rule_id": "ARCH-PAR-1",
            "severity": "info", "snippet": "", "detail": "Split between two options.",
            "status": "active", "kind": "no_convention",
            "observed_options": [
                {"option_id": "par", "label": "Run in parallel", "sites": 2},
                {"option_id": "seq", "label": "Sequential", "sites": 2}
            ]
        }],
        "conventions": [serde_json::to_value(split_record()).expect("json")],
    })
}

/// The export gate of the stub server: held for the split until a ruling is stored, and again
/// after it is withdrawn.
#[derive(Clone, Default)]
struct GateState(std::sync::Arc<std::sync::atomic::AtomicBool>);

struct GateAnswer(GateState);

impl wiremock::Respond for GateAnswer {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let decided = self.0 .0.load(std::sync::atomic::Ordering::SeqCst);
        let gate = camerata_api_types::review_view::ExportGateView {
            split_rules: if decided {
                Vec::new()
            } else {
                vec![camerata_api_types::review_view::SplitRuleHold {
                    repo: "me/api".into(),
                    rule_id: "ARCH-PAR-1".into(),
                }]
            },
            ..Default::default()
        };
        ResponseTemplate::new(200).set_body_json(gate)
    }
}

/// Answers a ruling: remembers that the rule is decided (or not) and returns the stored ruling.
struct RulingAnswer(GateState, bool);

impl wiremock::Respond for RulingAnswer {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        self.0 .0.store(self.1, std::sync::atomic::Ordering::SeqCst);
        ResponseTemplate::new(200).set_body_json(json!({"id": "std-000001"}))
    }
}

async fn stub_server_with_gate() -> (MockServer, GateState) {
    let state = GateState::default();
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/projects/active"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "p1", "name": "Acme", "repos": ["me/api"], "client_name": ""
        })))
        .mount(&server)
        .await;
    // The saved draft holds the finished inspection, so the screen shows its findings.
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "scan": split_report_json(), "audit": split_report_json(),
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/decisions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "decisions": [], "standings": [], "migrated_from_draft": true,
            "latest_run_id": "insp-1"
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/review/export-gate"))
        .respond_with(GateAnswer(state.clone()))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/standards"))
        .respond_with(RulingAnswer(state.clone(), true))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/standards/std-000001/revoke"))
        .respond_with(RulingAnswer(state.clone(), false))
        .mount(&server)
        .await;
    (server, state)
}

const SPLIT_LINE: &str = "1 split rule needs a decision: ARCH-PAR-1";

async fn no_convention_panel(server: &MockServer) -> (Mounted, camerata_test_env::EnvGuard) {
    let env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    REPORT.with(|r| *r.borrow_mut() = split_report_json());
    let mut m = Mounted::mount(screen);
    m.pump(400).await;
    m.pump(300).await;
    m.click("data-kind=No convention");
    m.until("the split is held and listed", |h| {
        h.contains(SPLIT_LINE) && h.contains("No choice yet")
    })
    .await;
    (m, env)
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn an_untouched_split_reads_no_choice_yet_and_claims_nothing_was_recorded() {
    let (server, _) = stub_server_with_gate().await;
    let (m, _env) = no_convention_panel(&server).await;
    let html = m.html();
    assert!(
        html.contains("This rule needs a decision before a client package can be exported."),
        "{html}"
    );
    assert!(
        !html.contains("Left to the client"),
        "an untouched row must not claim a decision: {html}"
    );
    assert!(posts(&server, "/api/projects/p1/standards")
        .await
        .is_empty());
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn leaving_a_split_to_the_client_posts_it_and_the_export_line_clears() {
    let (server, _) = stub_server_with_gate().await;
    let (mut m, _env) = no_convention_panel(&server).await;
    m.click("data-control=leave");
    let html = m
        .until("the line clears", |h| !h.contains(SPLIT_LINE))
        .await;
    let sent = posts(&server, "/api/projects/p1/standards").await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["choice"], json!("leave_to_client"));
    assert_eq!(sent[0]["repo"], "me/api");
    assert!(html.contains("Left to the client"), "{html}");
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn accepting_an_option_on_a_split_posts_it_and_the_row_and_line_follow() {
    let (server, _) = stub_server_with_gate().await;
    let (mut m, _env) = no_convention_panel(&server).await;
    m.pick("data-control=accept", "seq");
    let html = m
        .until("the line clears", |h| !h.contains(SPLIT_LINE))
        .await;
    let sent = posts(&server, "/api/projects/p1/standards").await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["choice"], json!({"accept": {"option_id": "seq"}}));
    assert!(html.contains("Accepted: Sequential"), "{html}");
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn withdrawing_a_ruling_on_a_split_returns_it_to_no_choice_yet() {
    let (server, _) = stub_server_with_gate().await;
    let (mut m, _env) = no_convention_panel(&server).await;
    m.pick("data-control=accept", "seq");
    m.until("the ruling shows with its withdraw", |h| {
        h.contains("Accepted: Sequential") && h.contains("Withdraw")
    })
    .await;
    m.click("data-control=withdraw");
    let html = m
        .until("it is a split with no choice again", |h| {
            h.contains("No choice yet") && h.contains(SPLIT_LINE)
        })
        .await;
    assert_eq!(
        posts(&server, "/api/projects/p1/standards/std-000001/revoke")
            .await
            .len(),
        1,
        "{html}"
    );
}

// ── Test 23, the whole of it: accept on a split, and the Queue lane shows the departures ──────

/// Answers the Queue lane's read: the rows as the scan held them until a ruling is stored, then the
/// reviewed rows with the departure the ruling made.
struct QueueAnswer {
    gate: GateState,
    before: Value,
    after: Value,
}

impl wiremock::Respond for QueueAnswer {
    fn respond(&self, _: &Request) -> ResponseTemplate {
        let decided = self.gate.0.load(std::sync::atomic::Ordering::SeqCst);
        ResponseTemplate::new(200).set_body_json(if decided { &self.after } else { &self.before })
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn accepting_on_a_split_posts_the_ruling_and_the_queue_lane_shows_the_departures() {
    use camerata_ui_core::test_support::review_fixtures::{build, Params};
    let (server, gate) = stub_server_with_gate().await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    // The review views: the shell, and the Queue lane before and after the ruling.
    let ds = build(&Params::base(7));
    let mut before = ds.queue(false, 0, 20);
    before.escalated.truncate(1);
    let mut after = before.clone();
    let mut departure = after.escalated[0].clone();
    departure.site.path = "src/departed-place.ts".into();
    departure.site.line = 11;
    departure.site.rule_id = "ARCH-PAR-1".into();
    departure.site.site_key = "departure-key".into();
    departure.candidate.key = "departure-key".into();
    after.escalated.push(departure);
    assert!(
        !serde_json::to_string(&before)
            .unwrap_or_default()
            .contains("departed-place"),
        "anti-vacuity"
    );
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/review"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&ds.shell))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/review/queue"))
        .respond_with(QueueAnswer {
            gate: gate.clone(),
            before: serde_json::to_value(&before).expect("json"),
            after: serde_json::to_value(&after).expect("json"),
        })
        .mount(&server)
        .await;
    REPORT.with(|r| *r.borrow_mut() = split_report_json());
    let mut m = Mounted::mount(screen);
    m.pump(400).await;
    m.pump(300).await;
    // The Review lanes, on the Queue lane.
    m.click("id=review-open");
    m.until("the review shell is up", |h| {
        h.contains("review-strip") || h.contains("Review progress")
    })
    .await;
    m.until("the lane tabs are drawn", |h| {
        h.contains("review-lane-escalations")
    })
    .await;
    m.click("id=review-lane-escalations");
    m.pump(400).await;
    assert!(!m.html().contains("departed-place"), "{}", m.html());
    // The hub on the split, and Accept.
    m.forget();
    let hub = m.context::<crate::cockpit::viewer::ViewerHubOpen>();
    m.vdom.in_runtime(|| {
        let mut sig = hub.0;
        sig.set(Some(RuleHubRequest {
            repo: "me/api".into(),
            rule_id: "ARCH-PAR-1".into(),
        }));
    });
    m.until("the hub shows the split", |h| h.contains("rule-hub-modal"))
        .await;
    m.click("data-control=accept");
    let html = m
        .until("the Queue shows the departure", |h| {
            h.contains("departed-place")
        })
        .await;
    let sent = posts(&server, "/api/projects/p1/standards").await;
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0]["choice"], json!({"accept": {"option_id": "par"}}));
    assert!(
        !html.contains("split rule needs a decision"),
        "the line no longer says the rule needs a decision: {html}"
    );
}
