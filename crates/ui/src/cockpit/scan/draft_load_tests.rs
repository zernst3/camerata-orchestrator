//! Loading the saved onboarding draft never turns a part it cannot read into "nothing saved":
//! the operator is told, in words, and the draft stays as it is on disk.

use super::*;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn scan() -> serde_json::Value {
    serde_json::json!({
        "repos": ["me/api"], "files_scanned": 1, "findings": [],
        "proposed_rules": [], "gated": false
    })
}

async fn serve(status: u16, body: serde_json::Value) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/onboard/draft"))
        .respond_with(ResponseTemplate::new(status).set_body_json(body))
        .mount(&server)
        .await;
    server
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_malformed_audit_is_an_error_naming_the_saved_review_not_no_draft() {
    // The audit holds `findings` as a number: it cannot be read as a report.
    let server = serve(
        200,
        serde_json::json!({ "scan": scan(), "audit": { "repos": ["me/api"], "findings": 7 } }),
    )
    .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let result = load_onboarding_draft().await;
    let Err(error) = result else {
        panic!("a draft whose audit cannot be read must not load as Ok");
    };
    assert!(error.unparseable_draft, "a draft exists that cannot be read");
    let message = error.message;
    assert!(
        message.starts_with("The saved review in your onboarding draft could not be read"),
        "{message}"
    );
    assert!(message.contains("nothing was deleted"), "{message}");
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn the_servers_own_refusal_is_shown_when_the_draft_file_was_unreadable() {
    let server = serve(
        409,
        serde_json::json!({ "ok": false, "code": "draft_unreadable",
            "message": "The saved onboarding draft file could not be read, so no earlier review was restored. The file was kept untouched at /x/onboarding-draft.json.unreadable. Nothing was deleted." }),
    )
    .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", server.uri());
    let error = load_onboarding_draft().await.err().expect("refused");
    assert!(
        !error.unparseable_draft,
        "a refused read is told once; saving goes on"
    );
    let message = error.message;
    assert!(
        message.contains("kept untouched at /x/onboarding-draft.json.unreadable"),
        "{message}"
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_draft_with_a_readable_audit_loads_it_and_a_null_audit_stays_none() {
    let with = serve(
        200,
        serde_json::json!({ "scan": scan(), "audit": { "repos": ["me/api"], "files_scanned": 1,
            "findings": [], "proposed_rules": [], "gated": false } }),
    )
    .await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", with.uri());
    let draft = load_onboarding_draft().await.unwrap().expect("a draft");
    assert!(draft.audit.is_some());
    drop(_env);

    let without = serve(200, serde_json::json!({ "scan": scan(), "audit": null })).await;
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_BFF_URL", without.uri());
    let draft = load_onboarding_draft().await.unwrap().expect("a draft");
    assert!(draft.audit.is_none());
}
