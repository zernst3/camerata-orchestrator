//! The acceptance test of #562: an imported command line run opens on the review screens with
//! the same rows, coverage and usage as an app scan of the same fixture.
//!
//! The app side stores the fixture the way the app's scan job does ([`AppState::set_last_scan`]
//! after the job assigned site keys); the command line side seals it the way `camerata inspect`
//! does, saves it with [`crate::run_store::write_report`] and imports it. Both are then read
//! back through the review routes, and through the stored entry, and compared whole (only the
//! inspection's own id differs, and it is replaced before comparing).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::*;

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes)
        .unwrap_or_else(|_| serde_json::Value::String(String::from_utf8_lossy(&bytes).into()));
    (status, value)
}

/// `value` with every occurrence of the inspection id `id` replaced by a placeholder.
fn without_id(value: &serde_json::Value, id: &str) -> serde_json::Value {
    serde_json::from_str(&value.to_string().replace(id, "<inspection>")).unwrap()
}

/// The review screens' reads of the project's latest inspection: the shell, the tree and the
/// sites of every node, with the inspection id taken out.
async fn review_screens(state: &AppState, project: &str) -> Vec<(String, serde_json::Value)> {
    let entry_id = state.inspections.latest(project).expect("an inspection").id;
    let app = crate::router(state.clone());
    let mut out = Vec::new();
    for uri in [
        format!("/api/projects/{project}/review"),
        format!("/api/projects/{project}/review/tree"),
        format!("/api/projects/{project}/review/queue"),
    ] {
        let (status, body) = get(&app, &uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        out.push((
            uri.replace(project, "<project>"),
            without_id(&body, &entry_id),
        ));
    }
    let tree = &out[1].1;
    let mut nodes: Vec<String> = tree["nodes"]
        .as_array()
        .expect("the tree has nodes")
        .iter()
        .filter_map(|n| n["id"].as_str().map(str::to_string))
        .collect();
    nodes.sort();
    assert!(!nodes.is_empty(), "the tree has nodes: {tree}");
    for node in nodes {
        let uri = format!(
            "/api/projects/{project}/review/sites?node={}&limit=500",
            urlencode(&node)
        );
        let (status, body) = get(&app, &uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        out.push((
            uri.replace(project, "<project>"),
            without_id(&body, &entry_id),
        ));
    }
    out
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_imported_run_opens_on_the_review_screens_like_an_app_scan_of_the_same_fixture() {
    // The app's own scan of the fixture.
    let scanned = app();
    let mut report = fixture_report();
    report.assign_site_keys();
    scanned
        .state
        .set_last_scan(scanned.project_id.clone(), report);

    // The command line's run of the same fixture, imported.
    let imported = app();
    assert_eq!(
        imported.project_id, scanned.project_id,
        "the two data folders give the project the same id"
    );
    let folder = saved_run(fixture_report());
    let done = import_cli_run(&imported.state, &request(&folder.dir));
    assert!(done.is_ok(), "the import succeeds: {done:?}");

    // The stored entries hold the same rows, coverage and usage.
    let a = scanned
        .state
        .inspections
        .latest(&scanned.project_id)
        .unwrap();
    let b = imported
        .state
        .inspections
        .latest(&imported.project_id)
        .unwrap();
    let rows = |e: &crate::inspections::InspectionEntry| {
        let mut rows: Vec<(String, String, usize, String, String)> = e
            .report
            .findings
            .iter()
            .map(|f| {
                (
                    f.site_key.clone(),
                    f.path.clone(),
                    f.line,
                    f.rule_id.clone(),
                    f.snippet.clone(),
                )
            })
            .collect();
        rows.sort();
        rows
    };
    assert_eq!(
        rows(&b),
        rows(&a),
        "the same rows, with the same keys and text"
    );
    assert_eq!(
        serde_json::to_value(&b.report.coverage_notes).unwrap(),
        serde_json::to_value(&a.report.coverage_notes).unwrap(),
        "the same coverage"
    );
    assert_eq!(
        b.report.actual_usage, a.report.actual_usage,
        "the same usage"
    );
    assert_eq!(b.repos, a.repos);
    assert_eq!(b.started_at, a.started_at);
    assert_eq!(b.rules_fingerprint, a.rules_fingerprint);

    // And the review screens read the same.
    let from_scan = review_screens(&scanned.state, &scanned.project_id).await;
    let from_import = review_screens(&imported.state, &imported.project_id).await;
    assert_eq!(from_import.len(), from_scan.len(), "the same screens");
    for ((uri, scan), (_, import)) in from_scan.iter().zip(&from_import) {
        assert_eq!(import, scan, "{uri} reads the same for the imported run");
    }
}
