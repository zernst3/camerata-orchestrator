//! Tests: order preset reload tests.

use super::*;
use std::time::Duration;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn scan_with(findings: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "repos": ["me/api"], "files_scanned": 1, "findings": findings,
        "proposed_rules": [], "gated": false
    })
}

fn findings() -> serde_json::Value {
    serde_json::json!([
        { "repo": "me/api", "path": "src/z.rs", "line": 40, "rule_id": "R-A",
          "severity": "high", "snippet": "z", "detail": "d", "status": "active",
          "kind": "defect" },
        { "repo": "me/api", "path": "src/a.rs", "line": 9, "rule_id": "R-B",
          "severity": "critical", "snippet": "a", "detail": "d", "status": "active",
          "kind": "defect" },
    ])
}

fn results() -> Element {
    use_context_provider(|| Signal::new(Vec::<crate::toast::Toast>::new()));
    use_context_provider(|| Signal::new(None::<ScanReportView>));
    use_context_provider(|| Signal::new(CockpitView::Stories));
    use_context_provider(|| Signal::new(None::<String>));
    use_context_provider(|| Signal::new(0_usize) as crate::loading::LoadingCount);
    let report: ScanReportView = serde_json::from_value(scan_with(serde_json::json!([])))
        .expect("the scan the draft belongs to");
    rsx! { ScanResults { report } }
}

async fn pump(vdom: &mut VirtualDom, ms: u64) {
    let until = tokio::time::Instant::now() + Duration::from_millis(ms);
    while tokio::time::Instant::now() < until {
        let _ = tokio::time::timeout(Duration::from_millis(10), vdom.wait_for_work()).await;
        let _ = vdom.render_immediate_to_vec();
    }
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_draft_that_names_a_preset_brings_the_queue_back_cut_that_way_and_saves_it_again() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "scan": scan_with(serde_json::json!([])),
            "audit": scan_with(findings()),
            "order_preset": "by_file",
        })))
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
            "id": "p1", "name": "Acme", "client_name": ""
        })))
        .mount(&server)
        .await;
    // The decision store answers (the Queue is drawn only while decisions are known).
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/decisions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "decisions": [], "standings": [], "migrated_from_draft": true,
            "latest_run_id": "insp-1"
        })))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

    let mut vdom = VirtualDom::new(results);
    vdom.rebuild_in_place();
    pump(&mut vdom, 800).await;
    let html = dioxus_ssr::render(&vdom);

    // The select shows the restored cut, and the Queue is grouped by file.
    let by_file = html
        .find("<option value=\"by_file\"")
        .unwrap_or_else(|| panic!("the Queue is not mounted: {html}"));
    let tag = &html[by_file..by_file + html[by_file..].find('>').unwrap()];
    assert!(tag.contains("selected"), "{tag}");
    assert!(html.contains(&OrderPreset::ByFile.sentence()), "{html}");
    assert!(
        html.contains("src/a.rs") && html.contains("src/z.rs"),
        "file groups: {html}"
    );

    // And the draft saved after the restore still carries it.
    let saved = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/onboard/draft")
        .map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).unwrap())
        .next_back()
        .expect("the restored view saved the draft");
    assert_eq!(saved["order_preset"], "by_file", "{saved}");
}

/// With the decision store unreachable (or its file damaged: the server answers 503
/// `store_corrupt`) the Queue is not drawn (its counts would read every
/// finding as Unresolved), and the banner says why.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn an_unreachable_decision_store_hides_the_queue_and_shows_the_banner() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "scan": scan_with(serde_json::json!([])),
            "audit": scan_with(findings()),
        })))
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
            "id": "p1", "name": "Acme", "client_name": ""
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/projects/p1/decisions"))
        // What the server answers for a project whose decision file is damaged.
        .respond_with(ResponseTemplate::new(503).set_body_json(serde_json::json!({
            "ok": false, "code": "store_corrupt", "message": "the decision file is damaged"
        })))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

    let mut vdom = VirtualDom::new(results);
    vdom.rebuild_in_place();
    pump(&mut vdom, 800).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(html.contains("Decisions not loaded"), "{html}");
    assert!(
        !html.contains("triage-switch"),
        "no Unresolved count is drawn while decisions are unknown"
    );
    // The draft saved while unknown does not overwrite the queue with an empty one.
    let saved = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/onboard/draft")
        .map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).unwrap())
        .next_back()
        .expect("the draft was saved");
    assert_eq!(saved["dispositions"], serde_json::json!({}));
    assert!(
        saved["last_known"].is_null(),
        "no state was ever read: {saved}"
    );
}

/// M1: two saves queued out of order (the second submitted while the first is still on the
/// wire, and a third behind it) reach the server one at a time and the NEWEST lands last. The
/// first response is slow on purpose; without the single in-flight slot the later ones would
/// overtake it and the older draft would be the one the server keeps.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn queued_saves_reach_the_server_in_order_and_the_newest_wins() {
    use std::cell::RefCell;
    use std::rc::Rc;
    let server = MockServer::start().await;
    // The first POST answers slowly; the rest at once.
    Mock::given(method("POST"))
        .and(path("/api/onboard/draft"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true}))
                .set_delay(Duration::from_millis(300)),
        )
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .with_priority(5)
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

    let draft = |mark: &str| -> OnboardingDraft {
        let mut d: OnboardingDraft = serde_json::from_value(serde_json::json!({
            "scan": scan_with(serde_json::json!([])),
            "viewed_repo": mark,
        }))
        .expect("draft");
        d.viewed_repo = mark.to_string();
        d
    };
    let queue = Rc::new(RefCell::new(camerata_ui_core::save_queue::SaveQueue::<
        OnboardingDraft,
    >::default()));
    let first = save_onboarding_draft_ordered(&queue, draft("first"), || {});
    let later = async {
        tokio::time::sleep(Duration::from_millis(50)).await;
        // Both arrive while the first is still in flight; the second is superseded.
        save_onboarding_draft_ordered(&queue, draft("second"), || {}).await;
        save_onboarding_draft_ordered(&queue, draft("third"), || {}).await;
    };
    tokio::join!(first, later);

    let bodies: Vec<String> = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/onboard/draft")
        .map(|r| {
            serde_json::from_slice::<serde_json::Value>(&r.body).unwrap()["viewed_repo"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(
        bodies,
        vec!["first", "third"],
        "one at a time; the superseded draft is never sent; the newest is last"
    );
}

/// M2, end to end: a saved review log with one unreadable entry loads (the good entries and
/// the time survive), the next save writes the readable log AND the original verbatim, and
/// the user is told.
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_draft_with_an_unreadable_review_entry_keeps_the_original_and_saves_it_back() {
    let good = "me/api\u{1f}R-B";
    let log = serde_json::json!({
        "review_groups": {
            good: { "opened_at_ms": 5, "active_ms": 72_000_000, "group_reviewed": false },
            "me/api\u{1f}R-A": { "active_ms": "twenty hours" },
        },
        "review_rows": {},
    });
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "scan": scan_with(serde_json::json!([])),
            "audit": scan_with(findings()),
            "review_log": log,
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

    let mut vdom = VirtualDom::new(results);
    vdom.rebuild_in_place();
    pump(&mut vdom, 800).await;

    let saved = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/onboard/draft")
        .map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).unwrap())
        .next_back()
        .expect("the restored view saved the draft");
    let saved_log = &saved["review_log"];
    assert_eq!(
        saved_log["review_groups"][good]["active_ms"], 72_000_000,
        "the readable entry survived: {saved}"
    );
    assert_eq!(
        saved_log["review_unreadable"], log,
        "the whole original is kept verbatim: {saved}"
    );
}

/// Step 7a: a draft's review log comes back with the review and is saved again, with each
/// group's reviewed flag refreshed from the dispositions (here: nothing is decided, so a
/// group the old log called reviewed is not).
#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn a_draft_brings_its_review_log_back_and_saves_it_with_fresh_group_flags() {
    let group = "me/api\u{1f}R-B";
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "scan": scan_with(serde_json::json!([])),
            "audit": scan_with(findings()),
            "review_log": {
                "review_groups": {
                    group: { "opened_at_ms": 5, "active_ms": 90_000, "group_reviewed": true }
                },
                "review_rows": {},
            },
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ok": true})))
        .mount(&server)
        .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());

    let mut vdom = VirtualDom::new(results);
    vdom.rebuild_in_place();
    pump(&mut vdom, 800).await;

    let saved = server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path() == "/api/onboard/draft")
        .map(|r| serde_json::from_slice::<serde_json::Value>(&r.body).unwrap())
        .next_back()
        .expect("the restored view saved the draft");
    let timing = &saved["review_log"]["review_groups"][group];
    assert_eq!(timing["active_ms"], 90_000, "the time survived: {saved}");
    assert_eq!(
        timing["group_reviewed"], false,
        "no row of the group is decided, whatever the old flag said: {saved}"
    );
}
