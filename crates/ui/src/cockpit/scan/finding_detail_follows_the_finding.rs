//! Tests: the finding-detail modal's two on-demand sections follow the finding they are given.
//!
//! The modal mounts `FindingCodeContext` and `FindingRecommendedFix` with a `key:` meant to give
//! each finding a fresh section. Dioxus honours `key` only among the children of a list, and
//! these two are plain children of the modal, so the key did nothing: the first finding's fetch
//! stayed on screen when the finding changed. The components now read their props reactively,
//! which these tests drive through the real components.

use super::{FindingCodeContext, FindingRecommendedFix};
use camerata_ui_core::triage::FindingView;
use dioxus::prelude::*;
use std::cell::RefCell;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

thread_local! {
    static SHOWN: RefCell<Option<Signal<(usize, String)>>> = const { RefCell::new(None) };
}

fn finding(line: usize, rule_id: &str) -> FindingView {
    serde_json::from_value(serde_json::json!({
        "repo": "me/api", "path": "src/x.rs", "line": line, "rule_id": rule_id,
        "severity": "high", "snippet": format!("snippet {line}"), "detail": "d",
        "status": "active", "kind": "defect"
    }))
    .expect("fixture")
}

fn app() -> Element {
    let shown = use_signal(|| (4_usize, "RULE-A".to_string()));
    SHOWN.with(|s| *s.borrow_mut() = Some(shown));
    let (line, rule_id) = shown();
    rsx! {
        FindingCodeContext { finding: finding(line, &rule_id) }
        FindingRecommendedFix { rule_id }
    }
}

async fn settle(vdom: &mut VirtualDom) {
    for _ in 0..4 {
        let _ =
            tokio::time::timeout(std::time::Duration::from_millis(60), vdom.wait_for_work()).await;
        let _ = vdom.render_immediate_to_vec();
    }
}

async fn serve() -> MockServer {
    let server = MockServer::start().await;
    let ok = |body: serde_json::Value| ResponseTemplate::new(200).set_body_json(body);
    for (line, text) in [("4", "alpha_code"), ("9", "beta_code")] {
        Mock::given(method("GET"))
            .and(path("/api/onboard/finding-context"))
            .and(query_param("line", line))
            .respond_with(ok(serde_json::json!({
                "status": "ok", "lines": [text], "start_line": line.parse::<usize>().unwrap(),
                "violation_line": line.parse::<usize>().unwrap(), "kind": "function",
                "matches_snippet": true, "source": "git",
                "commit": "a".repeat(40), "head_differs": false
            })))
            .mount(&server)
            .await;
    }
    for (rule, fix) in [("RULE-A", "fix for A"), ("RULE-B", "fix for B")] {
        Mock::given(method("GET"))
            .and(path("/api/onboard/finding-fix"))
            .and(query_param("rule_id", rule))
            .respond_with(ok(serde_json::json!({"fix": fix})))
            .mount(&server)
            .await;
    }
    server
}

#[tokio::test(flavor = "current_thread")]
#[serial_test::serial(process_env)]
async fn changing_the_finding_replaces_the_code_context_and_the_recommended_fix() {
    let server = serve().await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let mut vdom = VirtualDom::new(app);
    vdom.rebuild_in_place();
    settle(&mut vdom).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("alpha_code") && html.contains("fix for A"),
        "{html}"
    );

    let shown = SHOWN.with(|s| s.borrow().expect("the harness ran"));
    vdom.in_runtime(|| {
        let mut shown = shown;
        shown.set((9, "RULE-B".to_string()));
    });
    let _ = vdom.render_immediate_to_vec();
    settle(&mut vdom).await;
    let html = dioxus_ssr::render(&vdom);
    assert!(
        html.contains("beta_code") && !html.contains("alpha_code"),
        "the second finding's code context replaces the first's: {html}"
    );
    assert!(
        html.contains("fix for B") && !html.contains("fix for A"),
        "the second rule's fix replaces the first's: {html}"
    );
}
