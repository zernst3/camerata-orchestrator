#![allow(clippy::unwrap_used)]
//! Drift baseline at save time, through the real HTTP routes and a real on-disk corpus.
//!
//! The unit tests in `camerata-server`'s lib cover the pure pieces (`save_armed_to_project`,
//! `drift_for_projects`, `upsert_base_rules`). This file proves the ROUTE wiring: the import
//! route loads the corpus from `CAMERATA_CORPUS_PATH` and baselines rules that are new to the
//! project, while a rule the project already had keeps the baseline it was applied with even
//! though the import resends it without one.
//!
//! It lives in its own test binary (its own process) because it must point
//! `CAMERATA_CORPUS_PATH` at a temp corpus, and in the lib's test binary dozens of tests read
//! that variable.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use camerata_server::project::RuleSelection;
use camerata_server::{router, AppState};
use tower::ServiceExt;

fn write_corpus(dir: &std::path::Path, ids: &[&str]) {
    for id in ids {
        std::fs::write(
            dir.join(format!("{}.toml", id.to_lowercase())),
            format!(
                "id = \"{id}\"\ntitle = \"T\"\ndomain = \"universal\"\nenforcement = \"prose\"\n\
                 directive = \"Do {id}.\"\n"
            ),
        )
        .unwrap();
    }
}

async fn post_ruleset(state: &AppState, project_id: &str, body: serde_json::Value) -> StatusCode {
    router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/projects/{project_id}/ruleset"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn import_route_baselines_new_rules_and_keeps_existing_baselines() {
    let corpus_dir = tempfile::tempdir().unwrap();
    write_corpus(corpus_dir.path(), &["NEW-RULE-1", "KNOWN-RULE-1"]);
    let _corpus = camerata_test_env::EnvGuard::set("CAMERATA_CORPUS_PATH", corpus_dir.path());

    let state = AppState::seeded();
    let p = state
        .projects()
        .create("P", vec!["me/api".to_string()])
        .unwrap();
    state.projects().update(&p.id, |proj| {
        proj.ruleset.selections.push(RuleSelection {
            rule_id: "KNOWN-RULE-1".to_string(),
            repos: vec!["me/api".to_string()],
            applied_hash: Some("sha256:applied-earlier".to_string()),
            applied_directive: Some("Do it the way it was applied.".to_string()),
            applied_version: Some(1),
            ..Default::default()
        });
    });

    let status = post_ruleset(
        &state,
        &p.id,
        serde_json::json!({
            "selections": [
                { "rule_id": "KNOWN-RULE-1", "repos": ["me/api"] },
                { "rule_id": "NEW-RULE-1", "repos": ["me/api"] },
            ]
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let sels = state.projects().get(&p.id).unwrap().ruleset.selections;
    let known = sels.iter().find(|s| s.rule_id == "KNOWN-RULE-1").unwrap();
    assert_eq!(
        known.applied_hash.as_deref(),
        Some("sha256:applied-earlier"),
        "an import that resends the rule without a baseline must not erase the one it has"
    );
    assert_eq!(
        known.applied_directive.as_deref(),
        Some("Do it the way it was applied.")
    );
    assert_eq!(known.applied_version, Some(1));

    let new = sels.iter().find(|s| s.rule_id == "NEW-RULE-1").unwrap();
    assert!(
        new.applied_hash
            .as_deref()
            .is_some_and(|h| h.starts_with(camerata_rules::RULE_HASH_PREFIX)),
        "a rule new to the project is baselined from the corpus: {new:?}"
    );
    assert!(
        new.applied_directive.is_some(),
        "the directive is stored too"
    );
}

#[tokio::test]
#[serial_test::serial(process_env)]
async fn import_route_without_a_corpus_invents_no_baseline() {
    let missing = tempfile::tempdir().unwrap().path().join("no-such-corpus");
    let _corpus = camerata_test_env::EnvGuard::set("CAMERATA_CORPUS_PATH", &missing);

    let state = AppState::seeded();
    let p = state
        .projects()
        .create("P", vec!["me/api".to_string()])
        .unwrap();
    let status = post_ruleset(
        &state,
        &p.id,
        serde_json::json!({ "selections": [{ "rule_id": "NEW-RULE-1", "repos": ["me/api"] }] }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let sels = state.projects().get(&p.id).unwrap().ruleset.selections;
    assert!(
        sels[0].applied_hash.is_none(),
        "no corpus means no baseline, never a fabricated one"
    );
}
