//! The remediation routes end to end: plan preview, create, idempotence, the scan guard, the
//! client-lock notice. No agent runs; the units are only created.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use camerata_worktracker::InMemoryStoryStore;
use http_body_util::BodyExt;
use tower::ServiceExt;

use crate::inspections::{InspectionEntry, InspectionKind};
use crate::onboard::{AuditedRef, Finding, ScanReport};
use crate::report_export::finding_key;
use crate::uow::BranchMode;
use crate::{router, AppState};

const SQL: &str = "SEC-NO-RAW-SQL-CONCAT-1";
const OTHER: &str = "AGENTIC-NO-TEST-TAMPER-1";

async fn send(app: &Router, uri: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

fn finding(rule: &str, path: &str, line: usize, snippet: &str) -> Finding {
    Finding {
        repo: "acme/api".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: "high".to_string(),
        snippet: snippet.to_string(),
        ..Finding::default()
    }
}

/// A state with one project listing `acme/api` and one recorded full inspection holding
/// `findings`. Returns the state, the project id and the inspection id.
fn seeded(findings: &[Finding]) -> (AppState, String, String) {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("P", vec!["acme/api".to_string()])
        .expect("project");
    let mut report = ScanReport::gated(&["acme/api".to_string()]);
    report.gated = false;
    report.findings = findings.to_vec();
    report.provenance.audited_refs = vec![AuditedRef {
        repo: "acme/api".to_string(),
        sha: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
        ..AuditedRef::default()
    }];
    report.provenance.rules_fingerprint = "rules-1".to_string();
    let entry = state
        .inspections
        .append(InspectionEntry::from_report(
            &project.id,
            report,
            InspectionKind::Full,
        ))
        .expect("inspection appended");
    (state, project.id, entry.id)
}

/// Every given finding Confirmed.
fn confirmed(findings: &[Finding]) -> serde_json::Value {
    let map: HashMap<String, serde_json::Value> = findings
        .iter()
        .map(|f| (finding_key(f), serde_json::json!({ "state": "Confirmed" })))
        .collect();
    serde_json::to_value(map).unwrap()
}

fn three_findings() -> Vec<Finding> {
    vec![
        finding(SQL, "src/db.rs", 42, "query(\"select \" + id)"),
        finding(SQL, "src/db.rs", 80, "query(\"delete \" + name)"),
        finding(OTHER, "tests/web.rs", 7, "assert!(true)"),
    ]
}

#[tokio::test]
async fn plan_with_no_inspection_is_404_and_creates_nothing() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("P", vec!["acme/api".to_string()])
        .unwrap();
    let app = router(state.clone());
    for route in ["plan", "create"] {
        let (status, body) = send(
            &app,
            &format!("/api/projects/{}/remediation/{route}", project.id),
            serde_json::json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}: {body}");
        assert_eq!(
            body["error"],
            "This project has no recorded inspection. Run an audit first."
        );
    }
    assert!(state.uow.list().is_empty(), "nothing was created");

    let (status, body) = send(
        &app,
        "/api/projects/proj-nope/remediation/plan",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["error"], "no such project");
}

#[tokio::test]
async fn plan_previews_the_units_and_counts_what_it_left_out_and_creates_nothing() {
    let findings = three_findings();
    let (state, project_id, inspection_id) = seeded(&findings);
    let app = router(state.clone());
    // Confirm two; leave the third untouched (unresolved).
    let mut dispositions = confirmed(&findings[..2]);
    dispositions[finding_key(&findings[2])] = serde_json::json!({ "state": "FalsePositive" });
    let (status, body) = send(
        &app,
        &format!("/api/projects/{project_id}/remediation/plan"),
        serde_json::json!({ "dispositions": dispositions }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["ok"], true);
    assert_eq!(body["inspection_id"], inspection_id.as_str());
    assert_eq!(body["audited"][0]["repo"], "acme/api");
    let units = body["units"].as_array().unwrap();
    assert_eq!(units.len(), 1, "{body}");
    assert_eq!(units[0]["rule_ids"][0], SQL);
    assert_eq!(units[0]["files"], 1);
    assert_eq!(units[0]["findings"].as_array().unwrap().len(), 2);
    assert_eq!(units[0]["already_covered_by"], serde_json::Value::Null);
    assert_eq!(body["skipped"]["false_positive"], 1);
    assert_eq!(body["skipped"]["unresolved"], 0);
    assert!(state.uow.list().is_empty(), "a preview creates nothing");

    // A new project is a client engagement: the notice says units cannot run yet.
    let lock = body["development_lock"].as_str().expect("a lock notice");
    assert!(lock.contains("client engagement"), "{lock}");
    // Turning the engagement off clears it.
    state
        .projects
        .update(&project_id, |p| p.client_engagement = false)
        .unwrap();
    let (_, body) = send(
        &app,
        &format!("/api/projects/{project_id}/remediation/plan"),
        serde_json::json!({ "dispositions": dispositions }),
    )
    .await;
    assert_eq!(body["development_lock"], serde_json::Value::Null);
}

#[tokio::test]
async fn create_makes_one_native_unit_per_planned_group_with_source_story_intake_scope_and_branch()
{
    let findings = three_findings();
    let (state, project_id, inspection_id) = seeded(&findings);
    let app = router(state.clone());
    let (status, body) = send(
        &app,
        &format!("/api/projects/{project_id}/remediation/create"),
        serde_json::json!({ "dispositions": confirmed(&findings) }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let created = body["created"].as_array().unwrap();
    assert_eq!(created.len(), 2, "{body}");

    let mut lens = Vec::new();
    for c in created {
        let id = c["uow_id"].as_str().unwrap();
        let unit = state.uow.get(id).expect("persisted");
        assert_eq!(unit.repo(), Some("acme/api"));
        assert!(unit.native_number.is_some());
        assert_eq!(unit.project_id.as_deref(), Some(project_id.as_str()));
        let src = unit.findings_source().expect("a findings source");
        assert_eq!(src.inspection_id, inspection_id);
        assert_eq!(src.audited[0].repo, "acme/api");
        assert_eq!(
            src.audited[0].sha.as_deref(),
            Some("0123456789abcdef0123456789abcdef01234567")
        );
        lens.push((src.rule_ids[0].clone(), src.findings.len()));

        // Intake scope: the unit's repository on a new branch named from its rule.
        assert_eq!(unit.intake.repos.len(), 1);
        let BranchMode::NewFromBase { new_name, .. } = &unit.intake.repos[0].branch else {
            panic!("a new branch: {:?}", unit.intake.repos[0].branch);
        };
        let slug = src.rule_ids[0].to_ascii_lowercase();
        assert_eq!(new_name, &format!("camerata/remediation/{slug}"));
        assert_eq!(c["branch"], new_name.as_str());
        assert!(
            unit.intake.context.contains(&inspection_id),
            "{}",
            unit.intake.context
        );
        assert!(unit.intake.context.contains("0123456"));

        // The story behind it names the rule and every finding's file and line.
        let story = state.stories.get(id).await.unwrap().expect("spine story");
        assert!(story.title.contains(&src.rule_ids[0]), "{}", story.title);
        assert!(story.title.contains("acme/api"), "{}", story.title);
        assert!(story.description.contains("## Rules to satisfy"));
        assert!(story.description.contains(&format!(
            "## Findings to close (inspection {inspection_id}, acme/api at 0123456789abcdef"
        )));
        for f in &src.findings {
            assert!(
                story
                    .description
                    .contains(&format!("| {} | {} | {} |", f.path, f.line, f.rule_id)),
                "{}",
                story.description
            );
            assert!(story.description.contains(&f.fingerprint));
            // Snippets stay out of the story; the agent reads the real file.
            assert!(!story.description.contains(&f.snippet));
        }
        assert!(story.description.contains("Close every row."));
        assert!(
            unit.history.iter().any(|h| h.kind == "created"),
            "{:?}",
            unit.history
        );
    }
    lens.sort();
    assert_eq!(
        lens,
        vec![(OTHER.to_string(), 1), (SQL.to_string(), 2)],
        "two units of 1 and 2 findings"
    );
}

#[tokio::test]
async fn create_is_idempotent_a_second_call_reports_the_units_as_already_covered() {
    let findings = three_findings();
    let (state, project_id, _) = seeded(&findings);
    let app = router(state.clone());
    let uri = format!("/api/projects/{project_id}/remediation/create");
    let body = serde_json::json!({ "dispositions": confirmed(&findings) });
    let (_, first) = send(&app, &uri, body.clone()).await;
    assert_eq!(first["created"].as_array().unwrap().len(), 2);
    let before = state.uow.list().len();

    let (status, second) = send(&app, &uri, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{second}");
    assert!(second["created"].as_array().unwrap().is_empty(), "{second}");
    assert_eq!(second["skipped_existing"].as_array().unwrap().len(), 2);
    assert_eq!(state.uow.list().len(), before, "no new unit");

    // The preview says the same thing.
    let (_, plan) = send(
        &app,
        &format!("/api/projects/{project_id}/remediation/plan"),
        body,
    )
    .await;
    for u in plan["units"].as_array().unwrap() {
        assert!(u["already_covered_by"].is_string(), "{plan}");
        assert!(u["already_covered_label"]
            .as_str()
            .unwrap()
            .starts_with("uow-"));
    }
}

#[tokio::test]
async fn a_new_confirmed_finding_after_a_first_create_gets_only_its_own_unit() {
    let mut findings = three_findings();
    let (state, project_id, _) = seeded(&findings[..2]);
    let app = router(state.clone());
    let uri = format!("/api/projects/{project_id}/remediation/create");
    let (_, first) = send(
        &app,
        &uri,
        serde_json::json!({ "dispositions": confirmed(&findings[..2]) }),
    )
    .await;
    assert_eq!(first["created"].as_array().unwrap().len(), 1);

    // A later inspection finds one more violation of the same rule in another file.
    findings.truncate(2);
    findings.push(finding(SQL, "src/more.rs", 3, "query(\"drop \" + t)"));
    let mut report = ScanReport::gated(&["acme/api".to_string()]);
    report.gated = false;
    report.findings = findings.clone();
    report.provenance.audited_refs = vec![AuditedRef {
        repo: "acme/api".to_string(),
        sha: Some("fedcba".to_string()),
        ..AuditedRef::default()
    }];
    state
        .inspections
        .append(InspectionEntry::from_report(
            &project_id,
            report,
            InspectionKind::Full,
        ))
        .unwrap();
    let (status, second) = send(
        &app,
        &uri,
        serde_json::json!({ "dispositions": confirmed(&findings) }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{second}");
    let created = second["created"].as_array().unwrap();
    assert_eq!(created.len(), 1, "{second}");
    let unit = state
        .uow
        .get(created[0]["uow_id"].as_str().unwrap())
        .unwrap();
    let src = unit.findings_source().unwrap();
    assert_eq!(src.findings.len(), 1, "only the uncovered finding");
    assert_eq!(src.findings[0].path, "src/more.rs");
    // Its branch does not reuse the first unit's.
    assert_eq!(
        created[0]["branch"],
        "camerata/remediation/sec-no-raw-sql-concat-1-2"
    );
    assert!(second["notes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n.as_str().unwrap().contains("already belong to")));
}

#[tokio::test]
async fn units_that_edit_the_same_file_record_the_earlier_unit_as_a_dependency() {
    let findings = vec![
        finding(SQL, "src/shared.rs", 1, "a"),
        finding(SQL, "src/shared.rs", 2, "b"),
        finding(OTHER, "src/shared.rs", 9, "c"),
    ];
    let (state, project_id, _) = seeded(&findings);
    let app = router(state.clone());
    let (_, body) = send(
        &app,
        &format!("/api/projects/{project_id}/remediation/create"),
        serde_json::json!({ "dispositions": confirmed(&findings) }),
    )
    .await;
    let created = body["created"].as_array().unwrap();
    assert_eq!(created.len(), 2, "{body}");
    let first = state
        .uow
        .get(created[0]["uow_id"].as_str().unwrap())
        .unwrap();
    let second = state
        .uow
        .get(created[1]["uow_id"].as_str().unwrap())
        .unwrap();
    assert!(first.findings_source().unwrap().depends_on.is_empty());
    assert_eq!(
        second.findings_source().unwrap().depends_on,
        vec![first.story_id.clone()],
        "the later unit names the earlier one by its story id"
    );
}

#[tokio::test]
async fn findings_a_shared_fix_group_absorbed_stay_on_the_unit() {
    // One location violates two rules; the scanner folded the second into also_matches and the
    // retest later sees the demoted row. The merged finding is its own source finding.
    let mut primary = finding(SQL, "src/db.rs", 10, "shared line");
    primary.also_matches = vec![OTHER.to_string()];
    let secondary = finding(OTHER, "src/db.rs", 10, "shared line");
    let findings = vec![primary, secondary];
    let (state, project_id, _) = seeded(&findings);
    let app = router(state.clone());
    let (_, body) = send(
        &app,
        &format!("/api/projects/{project_id}/remediation/create"),
        serde_json::json!({ "dispositions": confirmed(&findings) }),
    )
    .await;
    let created = body["created"].as_array().unwrap();
    assert_eq!(created.len(), 1, "one unit covers both rules: {body}");
    let unit = state
        .uow
        .get(created[0]["uow_id"].as_str().unwrap())
        .unwrap();
    let src = unit.findings_source().unwrap();
    assert!(src.rule_ids.contains(&SQL.to_string()) && src.rule_ids.contains(&OTHER.to_string()));
    let rules: Vec<&str> = src.findings.iter().map(|f| f.rule_id.as_str()).collect();
    assert!(
        rules.contains(&SQL) && rules.contains(&OTHER),
        "both findings stay traceable: {rules:?}"
    );
    // The absorbed finding points at the one that stands for the group, and only it.
    let primary = src
        .findings
        .iter()
        .find(|f| f.merged_into.is_none())
        .expect("a primary finding");
    let absorbed: Vec<_> = src
        .findings
        .iter()
        .filter(|f| f.merged_into.is_some())
        .collect();
    assert_eq!(absorbed.len(), 1, "{:?}", src.findings);
    assert_eq!(
        absorbed[0].merged_into.as_deref(),
        Some(primary.key.as_str())
    );
    assert_eq!(
        src.rules_fingerprint, "rules-1",
        "the inspection's rule fingerprint"
    );

    // The PR body lists the absorbed finding under its primary and counts both.
    let body = crate::remediation_plan::pr_body(src, &HashMap::new());
    let lines: Vec<&str> = body.lines().collect();
    let at = lines
        .iter()
        .position(|l| l.contains(&primary.fingerprint))
        .expect("the primary row");
    assert!(lines[at + 1].contains(&absorbed[0].fingerprint), "{body}");
    assert!(
        lines[at + 1].contains("merged into the row above"),
        "{body}"
    );
    assert!(body.contains("\n2 findings."), "{body}");
}

#[tokio::test]
async fn a_held_claim_refuses_a_create_and_a_released_one_lets_it_through() {
    let findings = three_findings();
    let (state, project_id, _) = seeded(&findings);
    let app = router(state.clone());
    let uri = format!("/api/projects/{project_id}/remediation/create");
    let body = serde_json::json!({ "dispositions": confirmed(&findings) });
    let claim = state
        .source_ops
        .try_begin(&project_id, "remediation:create")
        .expect("the claim is free");
    let (status, refused) = send(&app, &uri, body.clone()).await;
    assert_eq!(status, StatusCode::CONFLICT, "{refused}");
    assert_eq!(refused["error"], "remediation_in_progress");
    assert!(state.uow.list().is_empty());
    drop(claim);
    let (status, done) = send(&app, &uri, body.clone()).await;
    assert_eq!(status, StatusCode::OK, "{done}");
    assert_eq!(done["created"].as_array().unwrap().len(), 2);
    // The claim is released when the handler finishes, success or not.
    let (status, _) = send(&app, &uri, body).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_overlapping_creates_make_one_set_of_units() {
    let findings = three_findings();
    let (state, project_id, _) = seeded(&findings);
    let app = router(state.clone());
    let uri = format!("/api/projects/{project_id}/remediation/create");
    let body = serde_json::json!({ "dispositions": confirmed(&findings) });
    let calls: Vec<_> = (0..4)
        .map(|_| {
            let (app, uri, body) = (app.clone(), uri.clone(), body.clone());
            tokio::spawn(async move { send(&app, &uri, body).await })
        })
        .collect();
    let mut created = 0;
    for call in calls {
        let (status, json) = call.await.unwrap();
        assert!(
            status == StatusCode::OK || status == StatusCode::CONFLICT,
            "{status}: {json}"
        );
        created += json["created"].as_array().map_or(0, Vec::len);
    }
    assert_eq!(created, 2, "one set, however the calls overlapped");
    assert_eq!(state.uow.list().len(), 2);
}

#[tokio::test]
async fn create_refuses_while_a_scan_runs_for_the_project() {
    let findings = three_findings();
    let (state, project_id, _) = seeded(&findings);
    let _running = state.jobs.create("audit", Some(project_id.clone()));
    let app = router(state.clone());
    let (status, body) = send(
        &app,
        &format!("/api/projects/{project_id}/remediation/create"),
        serde_json::json!({ "dispositions": confirmed(&findings) }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["error"], "scan_in_progress");
    assert!(state.uow.list().is_empty());
    // A preview is still allowed: it creates nothing.
    let (status, _) = send(
        &app,
        &format!("/api/projects/{project_id}/remediation/plan"),
        serde_json::json!({ "dispositions": confirmed(&findings) }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn create_refuses_a_repository_outside_the_project_and_creates_nothing() {
    let findings = three_findings();
    let (state, project_id, _) = seeded(&findings);
    // The project no longer lists the repository the inspection read.
    state
        .projects
        .update(&project_id, |p| p.repos = vec!["acme/other".to_string()])
        .unwrap();
    let app = router(state.clone());
    let (status, body) = send(
        &app,
        &format!("/api/projects/{project_id}/remediation/create"),
        serde_json::json!({ "dispositions": confirmed(&findings) }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["error"].as_str().unwrap().contains("acme/api"));
    assert!(state.uow.list().is_empty(), "nothing half created");
}

#[tokio::test]
async fn the_inspection_id_picks_an_older_inspection() {
    let findings = three_findings();
    let (state, project_id, first_id) = seeded(&findings);
    // A newer, empty inspection becomes the latest.
    let mut report = ScanReport::gated(&["acme/api".to_string()]);
    report.gated = false;
    report.provenance.audited_refs = vec![AuditedRef {
        repo: "acme/api".to_string(),
        sha: Some("aaa".to_string()),
        ..AuditedRef::default()
    }];
    state
        .inspections
        .append(InspectionEntry::from_report(
            &project_id,
            report,
            InspectionKind::Full,
        ))
        .unwrap();
    let app = router(state.clone());
    let uri = format!("/api/projects/{project_id}/remediation/plan");
    let (_, latest) = send(
        &app,
        &uri,
        serde_json::json!({ "dispositions": confirmed(&findings) }),
    )
    .await;
    assert!(latest["units"].as_array().unwrap().is_empty());
    let (_, older) = send(
        &app,
        &uri,
        serde_json::json!({ "inspection_id": first_id, "dispositions": confirmed(&findings) }),
    )
    .await;
    assert_eq!(older["inspection_id"], first_id.as_str());
    assert_eq!(older["units"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn the_units_list_badges_a_unit_made_from_findings() {
    let findings = three_findings();
    let (state, project_id, _) = seeded(&findings);
    state.projects.set_active(&project_id);
    let app = router(state.clone());
    let (_, body) = send(
        &app,
        &format!("/api/projects/{project_id}/remediation/create"),
        serde_json::json!({ "dispositions": confirmed(&findings) }),
    )
    .await;
    let id = body["created"][0]["uow_id"].as_str().unwrap().to_string();
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/uows")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let list: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let row = list["uows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == id.as_str())
        .expect("listed");
    assert_eq!(row["source_kind"], "findings");
}
