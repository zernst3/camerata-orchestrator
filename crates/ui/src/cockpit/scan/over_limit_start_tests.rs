//! Item 31, the pre-start check: a file over the read limit that the project does not exclude by
//! agreement is listed on the results screen as a warning, Start stays on, and "Exclude by agreement" on the
//! file releases it. These tests mount the real results screen against a stub server and click
//! the real button.

use super::*;
use dioxus::dioxus_core::{AttributeValue, ElementId, Mutation, Mutations};
use std::time::Duration;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
    .expect("the scan the screen shows");
    rsx! { ScanResults { report } }
}

/// What the real screen has put on the page: each element's `id` attribute and each click
/// listener, by element, so a test clicks one named button and nothing else.
#[derive(Default)]
struct Seen {
    ids: Vec<(String, ElementId)>,
    clicks: Vec<ElementId>,
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
                Mutation::NewEventListener { name, id } if name == "click" => self.clicks.push(*id),
                _ => {}
            }
        }
    }

    /// The element carrying the html id `wanted` that has a click listener, if any.
    fn clickable(&self, wanted: &str) -> Option<ElementId> {
        self.ids
            .iter()
            .rev()
            .filter(|(t, _)| t == wanted)
            .map(|(_, id)| *id)
            .find(|id| self.clicks.contains(id))
    }
}

async fn pump(vdom: &mut VirtualDom, seen: &mut Seen, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        seen.take(&vdom.render_immediate_to_vec());
    }
}

fn click(vdom: &mut VirtualDom, id: ElementId) {
    use std::any::Any;
    use std::rc::Rc;
    let data: Box<dyn Any> = Box::new(dioxus::html::SerializedMouseData::default());
    let platform: Rc<dyn Any> = Rc::new(dioxus::html::PlatformEventData::new(data));
    vdom.runtime()
        .handle_event("click", dioxus::dioxus_core::Event::new(platform, true), id);
}

async fn stub_screen(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "scan": {
                "repos": ["me/api"], "files_scanned": 1, "findings": [],
                "proposed_rules": [], "gated": false
            },
            "audit": null,
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
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "p1", "name": "Api", "repos": ["me/api"]
        })))
        .mount(server)
        .await;
}

fn sizes(over_limit: serde_json::Value) -> serde_json::Value {
    sizes_with(over_limit, serde_json::json!([]))
}

fn sizes_with(over_limit: serde_json::Value, unmatched: serde_json::Value) -> serde_json::Value {
    serde_json::json!([{
        "repo": "me/api", "files": 3, "code_chars": 900, "error": null,
        "agent_finder": null, "over_limit": over_limit, "read_limit": "1 MB",
        "secret_search_limit": "20 MB",
        "unmatched_patterns": unmatched
    }])
}

/// The opening tag of the Start button, read off the rendered page.
fn start_button_tag(html: &str) -> String {
    let at = html
        .find("Inspect code against selected rules")
        .unwrap_or_else(|| panic!("no Start button on the screen: {html}"));
    let open = html[..at]
        .rfind("<button")
        .expect("the button's opening tag");
    html[open..at].to_string()
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_planted_file_over_the_limit_warns_and_can_still_be_excluded_by_agreement() {
    let server = MockServer::start().await;
    stub_screen(&server).await;
    // The first preflight lists a 1.2 MB file; once it is excluded the next one lists nothing.
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(sizes(serde_json::json!([
                { "path": "seeds/data.js", "bytes": 1_258_291, "display": "api/seeds/data.js" }
            ]))),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sizes(serde_json::json!([]))))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/scan-exclusions/add"))
        .and(body_json(
            serde_json::json!({ "repo": "me/api", "path": "seeds/data.js" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true,
            "project": { "id": "p1", "name": "Api", "scan_exclusions": ["api/seeds/data\\.js"] }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));

    let mut vdom = VirtualDom::new(results);
    let mut seen = Seen::default();
    seen.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut seen, 1_200).await;

    // The file is listed with its repo, path and size, in the words the check uses, and Start is off.
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("1 file will not be read for the inspection (over the 1 MB file size limit). It is still searched for secrets by pattern checks, never by the model; one over 20 MB or not text is checked by name only. A start is not held."),
        "{html}"
    );
    assert!(html.contains("api/seeds/data.js (1.2 MB)"), "{html}");
    assert!(html.contains("Exclude by agreement"), "{html}");
    assert!(
        !start_button_tag(&html).contains("disabled"),
        "a file over the limit is a warning, so Start stays on: {}",
        start_button_tag(&html)
    );

    // The operator's real action: the button on the listed file.
    let button = seen
        .clickable(&format!(
            "{}0",
            crate::source_ui::OVER_LIMIT_EXCLUDE_ID_PREFIX
        ))
        .expect("an Exclude by agreement button with a click handler");
    click(&mut vdom, button);
    pump(&mut vdom, &mut seen, 1_200).await;

    // Nothing is left to resolve: the notice is gone and Start is on.
    let html = dioxus_ssr::render(&vdom);
    assert!(!html.contains("file size limit"), "{html}");
    assert!(!html.contains("Exclude by agreement"), "{html}");
    assert!(
        !start_button_tag(&html).contains("disabled"),
        "{}",
        start_button_tag(&html)
    );
    // `expect(1)` on the exclusion stub checks the exact file was sent when the server drops.
}

// Round 1 item 13: a file excluded beforehand never shows because the SERVER leaves it off the
// wire; that is tested where it happens (`preflight` tests and
// `client_source::exclusion_tests::the_preflight_route_...`), not with a stub that sends nothing.

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_failed_exclusion_keeps_the_file_listed() {
    let server = MockServer::start().await;
    stub_screen(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(sizes(serde_json::json!([
                { "path": "seeds/data.js", "bytes": 1_258_291, "display": "api/seeds/data.js" }
            ]))),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/scan-exclusions/add"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": false, "message": "acme/elsewhere is not a repo of this project"
        })))
        .expect(1)
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(results);
    let mut seen = Seen::default();
    seen.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut seen, 1_200).await;
    let button = seen
        .clickable(&format!(
            "{}0",
            crate::source_ui::OVER_LIMIT_EXCLUDE_ID_PREFIX
        ))
        .expect("the button");
    click(&mut vdom, button);
    pump(&mut vdom, &mut seen, 1_200).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("api/seeds/data.js (1.2 MB)"),
        "still listed: {html}"
    );
    assert!(
        !start_button_tag(&html).contains("disabled"),
        "{}",
        start_button_tag(&html)
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_pattern_that_matches_nothing_blocks_start_until_it_is_removed() {
    let server = MockServer::start().await;
    stub_screen(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sizes_with(
            serde_json::json!([]),
            serde_json::json!(["api/sedes/**"]),
        )))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sizes(serde_json::json!([]))))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/scan-exclusions/remove"))
        .and(body_json(serde_json::json!({ "pattern": "api/sedes/**" })))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true, "project": { "id": "p1", "name": "Api" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(results);
    let mut seen = Seen::default();
    seen.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut seen, 1_200).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("holds a pattern that matches no file"),
        "{html}"
    );
    assert!(html.contains("api/sedes/**"), "{html}");
    assert!(html.contains("Remove pattern"), "{html}");
    assert!(
        start_button_tag(&html).contains("disabled"),
        "{}",
        start_button_tag(&html)
    );
    let button = seen
        .clickable(&format!(
            "{}0",
            crate::source_ui::UNMATCHED_REMOVE_ID_PREFIX
        ))
        .expect("a Remove pattern button");
    click(&mut vdom, button);
    pump(&mut vdom, &mut seen, 1_200).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(!html.contains("Remove pattern"), "{html}");
    assert!(
        !start_button_tag(&html).contains("disabled"),
        "{}",
        start_button_tag(&html)
    );
}

/// Item 7 (gap closed): when two repos share a name, the notice shows the owner form and "Exclude
/// by agreement" sends the full `owner/repo` so the server writes the owner form.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_owner_form_is_shown_and_sent_when_two_repos_share_a_name() {
    let server = MockServer::start().await;
    stub_screen(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([
            { "repo": "me/api", "files": 1, "code_chars": 10, "over_limit": [], "read_limit": "1 MB" },
            { "repo": "other/api", "files": 1, "code_chars": 10, "read_limit": "1 MB",
              "over_limit": [{ "path": "seeds/data.js", "bytes": 1_258_291, "display": "other/api/seeds/data.js" }] }
        ])))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sizes(serde_json::json!([]))))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/projects/p1/scan-exclusions/add"))
        .and(body_json(
            serde_json::json!({ "repo": "other/api", "path": "seeds/data.js" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "ok": true, "project": { "id": "p1", "name": "Api" }
        })))
        .expect(1)
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(results);
    let mut seen = Seen::default();
    seen.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut seen, 1_200).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("other/api/seeds/data.js (1.2 MB)"), "{html}");
    let button = seen
        .clickable(&format!(
            "{}0",
            crate::source_ui::OVER_LIMIT_EXCLUDE_ID_PREFIX
        ))
        .expect("the button");
    click(&mut vdom, button);
    pump(&mut vdom, &mut seen, 1_200).await;
    // `expect(1)` on the add stub checks the body named the full owner/repo when the server drops.
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

/// Item 1 (Zach's ruling): a file over the limit never stops the run. With the warning on screen,
/// the real Start button is enabled and a real click sends the start request.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_file_over_the_limit_does_not_stop_the_run_from_starting() {
    let server = MockServer::start().await;
    stub_screen(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(sizes(serde_json::json!([
                { "path": "seeds/data.js", "bytes": 1_258_291, "display": "api/seeds/data.js" }
            ]))),
        )
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/audit/start"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"job_id": "j1"})))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    dioxus::html::set_event_converter(Box::new(dioxus::html::SerializedHtmlEventConverter));
    let mut vdom = VirtualDom::new(results_with_a_selected_rule);
    let mut seen = Seen::default();
    seen.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut seen, 1_200).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("will not be read for the inspection"),
        "{html}"
    );
    assert!(
        !start_button_tag(&html).contains("disabled"),
        "{}",
        start_button_tag(&html)
    );
    let start = seen
        .clickable(crate::cockpit::rules::RUN_INSPECTION_ID)
        .expect("the Start button has a click handler");
    click(&mut vdom, start);
    pump(&mut vdom, &mut seen, 1_500).await;
    let starts = server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/onboard/audit/start")
        .count();
    assert_eq!(starts, 1, "the click starts the run");
}

/// Gap 5: a file with a line longer than 97 KB is listed in the pre-start notice with its
/// reason, and Start stays on.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_file_with_a_long_line_is_listed_with_its_reason_and_start_stays_on() {
    let server = MockServer::start().await;
    stub_screen(&server).await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(sizes(serde_json::json!([
                { "path": "d/wide.js", "bytes": 150_000, "display": "api/d/wide.js",
                  "reason": "has a line longer than 97 KB" }
            ]))),
        )
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut vdom = VirtualDom::new(results);
    let mut seen = Seen::default();
    seen.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut seen, 1_200).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("api/d/wide.js (146 KB, has a line longer than 97 KB)"),
        "{html}"
    );
    assert!(
        html.contains("will not be read for the inspection"),
        "{html}"
    );
    assert!(
        !start_button_tag(&html).contains("disabled"),
        "{}",
        start_button_tag(&html)
    );
}

/// V1 item 32: files that declare themselves generated are counted with their lines in the
/// pre-start check, in the one phrase Coverage uses, and Start stays on. The numbers come from the
/// preflight wire exactly as the server sends them.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn generated_files_are_counted_with_their_lines_before_the_start_and_start_stays_on() {
    let server = MockServer::start().await;
    stub_screen(&server).await;
    let mut body = sizes(serde_json::json!([]));
    body[0]["generated_files"] = serde_json::json!(3);
    body[0]["generated_lines"] = serde_json::json!(12_345);
    body[0]["generated_paths"] = serde_json::json!([
        "Migrations/Init.Designer.cs",
        "Migrations/AppDbContextModelSnapshot.cs",
        "web/schema.ts"
    ]);
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut vdom = VirtualDom::new(results);
    let mut seen = Seen::default();
    seen.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut seen, 1_200).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("3 files (12,345 lines) will not be read for the inspection, generated: searched for secrets only, not billed."),
        "{html}"
    );
    // Round 1: the pre-start check names the files, as the command line does.
    assert!(
        html.contains("Generated: Migrations/Init.Designer.cs; Migrations/AppDbContextModelSnapshot.cs; web/schema.ts."),
        "{html}"
    );
    assert!(
        !start_button_tag(&html).contains("disabled"),
        "{}",
        start_button_tag(&html)
    );
}

/// Zach's ruling: a pattern in a never-read folder is a warning with a Remove pattern action,
/// and Start stays on.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_noise_folder_pattern_is_a_warning_with_remove_and_start_stays_on() {
    let server = MockServer::start().await;
    stub_screen(&server).await;
    let mut row = sizes(serde_json::json!([]));
    row[0]["warned_patterns"] = serde_json::json!(["api/node_modules/**"]);
    row[0]["warned_reasons"] =
        serde_json::json!(["node_modules is never read, so this pattern is not needed"]);
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(ResponseTemplate::new(200).set_body_json(row))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut vdom = VirtualDom::new(results);
    let mut seen = Seen::default();
    seen.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut seen, 1_200).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("matches no file: node_modules is never read, so this pattern is not needed"),
        "{html}"
    );
    assert!(html.contains("Remove pattern"), "{html}");
    assert!(
        !start_button_tag(&html).contains("disabled"),
        "{}",
        start_button_tag(&html)
    );
}

/// Round 2 item 1: a pattern naming a repo that was not read is listed as "cannot be checked",
/// with NO Remove pattern action, and Start stays off.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_pattern_for_an_unread_repo_has_no_remove_button_and_blocks_start() {
    let server = MockServer::start().await;
    stub_screen(&server).await;
    let mut row = sizes(serde_json::json!([]));
    row[0]["unchecked_patterns"] = serde_json::json!(["zzz/seeds/**"]);
    row[0]["unchecked_reasons"] = serde_json::json!(["cannot be checked: other/zzz was not read"]);
    Mock::given(method("POST"))
        .and(path("/api/onboard/preflight"))
        .respond_with(ResponseTemplate::new(200).set_body_json(row))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut vdom = VirtualDom::new(results);
    let mut seen = Seen::default();
    seen.take(&vdom.rebuild_to_vec());
    pump(&mut vdom, &mut seen, 1_200).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("zzz/seeds/** (cannot be checked: other/zzz was not read)"),
        "{html}"
    );
    assert!(
        html.contains("holds a pattern that cannot be checked"),
        "{html}"
    );
    assert!(!html.contains("Remove pattern"), "{html}");
    assert!(
        start_button_tag(&html).contains("disabled"),
        "{}",
        start_button_tag(&html)
    );
}
