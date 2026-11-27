//! #564: the pre-flight before Start, on the real inspection screen (`OnboardView` with
//! `ScanResults` inside it) over a stub server, driven with the operator's real controls.
//! "Inspect code against selected rules" fetches the plan and shows it; nothing is sent until
//! Start in the pre-flight panel; Start stays off when the server says it would refuse.

use serde_json::{json, Value};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::inspection_screen_tests::{
    mount_job, requests_to, rules_proposed, stub_server, tag_of, visible_text, Screen, Sizes,
};

const REFUSAL: &str = "This client inspection did not start: it needs a spend limit. Enter a \
                       spend limit above zero in US dollars (the pre-flight suggests one), then \
                       start again. Nothing was read and no model was called.";

/// A plan with figures the screen can be checked against.
fn plan(start_refusal: Option<&str>) -> Value {
    json!({
        "ok": true,
        "client_engagement": true,
        "plan": {
            "repos": [{
                "repo": "me/api", "files": 10, "lines": 1234, "bytes": 4000,
                "left_out": [{ "reason": "over the file size limit", "files": 2 }]
            }],
            "total_files": 10, "total_lines": 1234, "total_bytes": 4000,
            "projected_usd": 4.2,
            "full_scan": {
                "lines": 1100, "rate": 1.71, "from_flag": false, "point": 1.88,
                "low": 1.3, "high": 4.06, "limit": 2.82
            },
            "limit_usd": null,
            "warnings": ["warning: me/api/seeds/data.js is over the 1 MB file size limit"],
            "refusal": null,
            "text": "\n== ADMISSION (what a real run would read) ==\n  me/api: 10 files admitted\n"
        },
        "start_refusal": start_refusal,
    })
}

/// A plan Start may go ahead on, for the screen tests that press Start after Inspect.
pub(super) async fn mount_open_plan(server: &MockServer) {
    Mock::given(method("POST"))
        .and(path("/api/onboard/plan"))
        .respond_with(ResponseTemplate::new(200).set_body_json(plan(None)))
        .mount(server)
        .await;
}

/// This test's own plan, ahead of the open plan the shared stub server mounts.
async fn mount_plan(server: &MockServer, body: Value) {
    Mock::given(method("POST"))
        .and(path("/api/onboard/plan"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .with_priority(1)
        .mount(server)
        .await;
}

/// The screen with rules proposed, the spend limit typed and Inspect pressed.
async fn inspect_with_limit(limit: &str) -> Screen {
    let mut screen = Screen::mount(rules_proposed);
    screen.pump(900).await;
    screen.type_into("inspection-spend-limit", limit);
    screen.pump(200).await;
    screen.click_tour("run-inspection");
    screen.pump(1500).await;
    screen
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn inspect_shows_the_preflight_and_sends_nothing_until_start() {
    let (server, _) = stub_server("agent", Sizes::Back).await;
    mount_job(&server, "running").await;
    mount_plan(&server, plan(None)).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = inspect_with_limit("50").await;

    let asked = requests_to(&server, "/api/onboard/plan").await;
    assert_eq!(asked.len(), 1, "the plan is asked for once");
    assert_eq!(asked[0]["max_cost_usd"], json!(50.0), "{}", asked[0]);
    assert_eq!(asked[0]["repos"], json!(["me/api"]), "{}", asked[0]);
    assert!(
        requests_to(&server, "/api/onboard/audit/start")
            .await
            .is_empty(),
        "nothing is sent before Start"
    );
    let text = visible_text(&screen.html());
    for want in [
        "Pre-flight",
        "Lines: 1,234",
        "Files admitted: 10",
        "me/api: 10 files admitted, 1,234 lines; 2 left out (over the file size limit)",
        "Projected cost: about $4.20 (projected, API prices)",
        "Suggested spend limit: $2.82",
        "Warnings (1):",
        "me/api/seeds/data.js is over the 1 MB file size limit",
        "The plan as the command line prints it",
    ] {
        assert!(text.contains(want), "missing {want:?} in:\n{text}");
    }

    screen.click("preflight-start");
    screen.pump(1500).await;
    let started = requests_to(&server, "/api/onboard/audit/start").await;
    assert_eq!(started.len(), 1, "Start sends the inspection");
    assert_eq!(started[0]["max_cost_usd"], json!(50.0), "{}", started[0]);
    assert_eq!(
        started[0]["rules"], asked[0]["rules"],
        "the inspection sent is the one planned"
    );
    assert!(
        !screen.html().contains("id=\"inspection-preflight\""),
        "the panel closes once the inspection is sent"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_start_the_server_would_refuse_keeps_start_off_and_says_why() {
    let (server, _) = stub_server("agent", Sizes::Back).await;
    mount_job(&server, "running").await;
    mount_plan(&server, plan(Some(REFUSAL))).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = inspect_with_limit("").await;

    let html = screen.html();
    assert!(
        visible_text(&html).contains(REFUSAL),
        "the refusal is shown:\n{html}"
    );
    assert!(
        tag_of(&html, "preflight-start").contains("disabled"),
        "{}",
        tag_of(&html, "preflight-start")
    );
    screen.click("preflight-start");
    screen.pump(1000).await;
    assert!(
        requests_to(&server, "/api/onboard/audit/start")
            .await
            .is_empty(),
        "a start the server would refuse is not sent"
    );
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn back_closes_the_preflight_without_starting() {
    let (server, _) = stub_server("agent", Sizes::Back).await;
    mount_job(&server, "running").await;
    mount_plan(&server, plan(None)).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = inspect_with_limit("50").await;
    assert!(screen.html().contains("id=\"inspection-preflight\""));
    screen.click("preflight-back");
    screen.pump(500).await;
    assert!(
        !screen.html().contains("id=\"inspection-preflight\""),
        "Back closes the panel"
    );
    assert!(requests_to(&server, "/api/onboard/audit/start")
        .await
        .is_empty());
}

/// A plan is for the choices it was made with: changing the spend limit closes it, so a Start
/// never sends a limit the operator did not see the pre-flight for.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn changing_the_spend_limit_closes_the_preflight() {
    let (server, _) = stub_server("agent", Sizes::Back).await;
    mount_job(&server, "running").await;
    mount_plan(&server, plan(None)).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut screen = inspect_with_limit("50").await;
    assert!(screen.html().contains("id=\"inspection-preflight\""));
    screen.type_into("inspection-spend-limit", "5");
    screen.pump(300).await;
    assert!(
        !screen.html().contains("id=\"inspection-preflight\""),
        "a new limit needs a new pre-flight"
    );
    assert!(requests_to(&server, "/api/onboard/audit/start")
        .await
        .is_empty());
}

/// The panel's words follow the client text rules: no dashes, no serial comma.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn the_preflight_panel_has_no_dashes_and_no_serial_comma() {
    let (server, _) = stub_server("agent", Sizes::Back).await;
    mount_job(&server, "running").await;
    mount_plan(&server, plan(Some(REFUSAL))).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let screen = inspect_with_limit("").await;
    let html = screen.html();
    let at = html.find("id=\"inspection-preflight\"").expect("the panel");
    let panel = visible_text(&html[at..]);
    assert!(
        !panel.contains('\u{2014}') && !panel.contains('\u{2013}'),
        "{panel}"
    );
    assert!(!panel.contains(", and "), "{panel}");
}
