//! The standalone PDF route passes the same release gate as the package route (Fable wave 7,
//! round 2). `POST /audit-report` once ran only the secrets and unsynced checks, so a held row
//! (evidence unverified, a merge or a restore nobody decided, an Engine Check No nobody
//! corrected, a group decision missing a site, an incomplete run) did not stop a client PDF.
//!
//! Every held kind is run through both routes on the same state: the PDF route must refuse with
//! the same 422, naming the same sites, as the package route. The PDF route needs Typst to
//! compile a clear report, as the package route's own tests do; a refusal does not (the gate runs
//! before the compile only where the package route does, so these tests skip without Typst
//! exactly as the package route's do).

use std::collections::{BTreeSet, HashMap};

use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::review_view::{EngineCheckError, EngineCheckVerdictReq};
use serde_json::Value;

use super::tests::scan_report;
use super::CheckId;
use crate::onboard::{Finding, ScanReport};
use crate::report_export::overlay::{MERGED_INTO_CAPTURE, MERGED_LABEL_CAPTURE};

const NOW: &str = "2026-11-04T09:00:00Z";

fn typst_on_path() -> bool {
    camerata_test_env::typst_on_path()
}

fn second_row(path: &str, line: usize) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: path.to_string(),
        line,
        rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        severity: "high".to_string(),
        snippet: format!("format!(\"select {{}}\", x{line})"),
        detail: "Raw SQL \u{2014} The query is built from the request.".to_string(),
        ..Finding::default()
    }
}

fn request() -> axum::Json<crate::AuditReportReq> {
    axum::Json(crate::AuditReportReq {
        dispositions: HashMap::new(),
        options: crate::report_export::ReportOptions::default(),
    })
}

/// A state holding one client-engagement project with a client name and `report` as its last
/// scan. Returns the state and the project id.
fn state_with(report: ScanReport) -> (crate::AppState, String) {
    let state = crate::AppState::new(std::sync::Arc::new(crate::InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Acme", vec!["acme/portal".to_string()])
        .expect("project created");
    let project = state
        .projects
        .update(&project.id, |p| {
            p.client_name = "Example Client Ltd".to_string()
        })
        .expect("project exists");
    state.set_last_scan(project.id.clone(), report);
    (state, project.id)
}

fn site_key(report: &ScanReport, path: &str) -> String {
    report
        .clone()
        .seal()
        .findings
        .iter()
        .find(|f| f.path == path)
        .unwrap_or_else(|| panic!("no row in {path}"))
        .site_key
        .clone()
}

fn ctx() -> crate::decisions::DecisionCtx<'static> {
    crate::decisions::DecisionCtx {
        run_id: "insp-1",
        engine_version: "0.0.0-test",
        now: NOW,
    }
}

fn confirm_node() -> (String, NewDecisionReq) {
    let node = "rule:SEC-NO-RAW-SQL-CONCAT-1".to_string();
    (
        node.clone(),
        NewDecisionReq {
            scope: Scope::Node(node),
            key: String::new(),
            state: DecisionState::Confirmed,
            reason: "the same pattern in each".to_string(),
            decided_by: String::new(),
            needed_me: None,
            sample_verdict: None,
            except: Vec::new(),
            run_id: String::new(),
            decided_at: String::new(),
        },
    )
}

/// One held kind: the gate check that refuses it and how to build a state that holds it.
struct Held {
    name: &'static str,
    check: CheckId,
    build: fn() -> (crate::AppState, String),
}

fn evidence_unverified() -> (crate::AppState, String) {
    let mut report = scan_report();
    report.findings[0].captures.insert(
        crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "not found near the cited line".to_string(),
    );
    state_with(report)
}

fn junk_content() -> (crate::AppState, String) {
    let mut report = scan_report();
    report.findings[0].detail = "A title \u{2014} I cannot assist.".to_string();
    state_with(report)
}

fn merged_without_decision() -> (crate::AppState, String) {
    let mut report = scan_report();
    report.findings[0].captures.insert(
        MERGED_INTO_CAPTURE.to_string(),
        "some-target-key".to_string(),
    );
    report.findings[0].captures.insert(
        MERGED_LABEL_CAPTURE.to_string(),
        "Merged into src/other.rs:1".to_string(),
    );
    state_with(report)
}

fn restored_without_decision() -> (crate::AppState, String) {
    let mut report = scan_report();
    report.findings[0].captures.insert(
        crate::engine_discard::ENGINE_RESTORED_CAPTURE.to_string(),
        "its headline is a placeholder".to_string(),
    );
    state_with(report)
}

fn engine_check_no_without_correction() -> (crate::AppState, String) {
    let report = scan_report();
    let key = site_key(&report, "src/db.rs");
    let (state, id) = state_with(report);
    // The verdict belongs to the run being exported: the latest inspection, which recording the
    // scan made.
    let run_id = state.inspections.latest(&id).expect("the run").id;
    state
        .engine_check
        .record(
            &id,
            &key,
            &EngineCheckVerdictReq {
                right: false,
                wrong: Some(EngineCheckError::WrongSeverity),
                correction: None,
                by: "reviewer".to_string(),
            },
            &run_id,
            NOW,
        )
        .expect("the verdict is recorded");
    (state, id)
}

fn node_decision_missing_a_member() -> (crate::AppState, String) {
    let mut report = scan_report();
    report.findings.push(second_row("src/users.rs", 21));
    let keys = [
        site_key(&report, "src/db.rs"),
        site_key(&report, "src/users.rs"),
    ];
    let (mut state, id) = state_with(report);
    // The store never writes a group decision without a record for every site it covers, so the
    // gap is made the way it arises: a decisions file with one site's record missing, opened by
    // a store.
    let dir = tempfile::tempdir().expect("a temp dir");
    let written = crate::decisions::DecisionStore::at(dir.path().to_path_buf());
    let (node, req) = confirm_node();
    written
        .record_node(&id, &node, &keys, &req, ctx())
        .expect("the group decision is recorded");
    let file = written.file_for(&id).expect("a file-backed store");
    let mut stored: Value =
        serde_json::from_slice(&std::fs::read(&file).expect("the file")).expect("JSON");
    let records = stored["decisions"].as_array_mut().expect("records");
    let before = records.len();
    records.retain(|d| !(d["key"] == keys[1] && d["scope"] == "site"));
    assert_eq!(records.len(), before - 1, "one site record is removed");
    std::fs::write(&file, serde_json::to_vec(&stored).expect("JSON")).expect("rewritten");
    state.decisions = crate::decisions::DecisionStore::at(dir.path().to_path_buf());
    (state, id)
}

const HELD: [Held; 6] = [
    Held {
        name: "evidence unverified",
        check: CheckId::EvidenceUnverified,
        build: evidence_unverified,
    },
    Held {
        name: "junk content",
        check: CheckId::Content,
        build: junk_content,
    },
    Held {
        name: "merged without decision",
        check: CheckId::MergedWithoutDecision,
        build: merged_without_decision,
    },
    Held {
        name: "restored without decision",
        check: CheckId::RestoredWithoutDecision,
        build: restored_without_decision,
    },
    Held {
        name: "engine check no without correction",
        check: CheckId::EngineCheckNoWithoutCorrection,
        build: engine_check_no_without_correction,
    },
    Held {
        name: "node decision missing a member",
        check: CheckId::MergedWithoutDecision,
        build: node_decision_missing_a_member,
    },
];

async fn body_json(resp: axum::response::Response) -> Value {
    use http_body_util::BodyExt;
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    serde_json::from_slice(&bytes).expect("a JSON body")
}

/// (check, file, location) of every problem in a 422 body: the sites the gate named.
fn sites_named(body: &Value) -> BTreeSet<(String, String, String)> {
    body["problems"]
        .as_array()
        .expect("a problem list")
        .iter()
        .map(|p| {
            let s = |k: &str| p[k].as_str().unwrap_or_default().to_string();
            (s("check"), s("file"), s("location"))
        })
        .collect()
}

async fn pdf_route(state: crate::AppState, id: &str) -> axum::response::Response {
    crate::export_audit_report(
        axum::extract::State(state),
        axum::extract::Path(id.to_string()),
        request(),
    )
    .await
}

async fn package_route(state: crate::AppState, id: &str) -> axum::response::Response {
    crate::export_product(
        axum::extract::State(state),
        axum::extract::Path(id.to_string()),
        request(),
    )
    .await
}

/// The fixtures are real: the package route refuses each, naming the check that holds it.
#[tokio::test]
async fn each_held_fixture_is_refused_by_the_package_route() {
    if !typst_on_path() {
        eprintln!("held fixtures test: skipping, typst is not on PATH");
        return;
    }
    for held in HELD {
        let (state, id) = (held.build)();
        let package = package_route(state, &id).await;
        assert_eq!(
            package.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "{}: the package route must refuse",
            held.name
        );
        let package = body_json(package).await;
        assert!(
            sites_named(&package)
                .iter()
                .any(|(c, _, _)| c == held.check.label()),
            "{}: the package route names the {} check: {package}",
            held.name,
            held.check.label()
        );
    }
}

#[tokio::test]
async fn the_pdf_route_refuses_each_held_kind_the_package_route_refuses() {
    if !typst_on_path() {
        eprintln!("pdf route held test: skipping, typst is not on PATH");
        return;
    }
    let mut failures: Vec<String> = Vec::new();
    for held in HELD {
        let (state, id) = (held.build)();
        let package = body_json(package_route(state.clone(), &id).await).await;

        let pdf = pdf_route(state, &id).await;
        let status = pdf.status();
        let content_type = pdf
            .headers()
            .get(axum::http::header::CONTENT_TYPE)
            .map(|v| v.as_bytes().to_vec());
        if status != axum::http::StatusCode::UNPROCESSABLE_ENTITY {
            failures.push(format!(
                "{}: the PDF route answered {status}, not 422",
                held.name
            ));
            continue;
        }
        if content_type.as_deref() != Some(&b"application/json"[..]) {
            failures.push(format!("{}: a refusal is JSON, never a PDF", held.name));
        }
        let pdf = body_json(pdf).await;
        if pdf["gate"] != "release" || pdf["ok"] != false {
            failures.push(format!("{}: not the release 422: {pdf}", held.name));
        }
        if sites_named(&pdf) != sites_named(&package) {
            failures.push(format!(
                "{}: the PDF route names {:?}, the package route {:?}",
                held.name,
                sites_named(&pdf),
                sites_named(&package)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "the PDF route must refuse what the package route refuses:\n  {}",
        failures.join("\n  ")
    );
}

#[tokio::test]
async fn the_pdf_route_refuses_an_incomplete_run_for_a_client() {
    use crate::ai_audit::{FailedPass, FailureClass};
    let mut report = scan_report();
    report.failed_passes = vec![FailedPass {
        repo: "acme/portal".to_string(),
        pass: "audit pass[rust] 1/2 \u{b7} rules 1/1".to_string(),
        reason: "Anthropic API HTTP 529: overloaded".to_string(),
        files_not_reviewed: 3,
        class: FailureClass::Transport,
        files: vec!["src/db.rs".to_string()],
        attempts: Vec::new(),
        scope_note: false,
    }];
    let (state, id) = state_with(report);
    let pdf = pdf_route(state, &id).await;
    assert_eq!(pdf.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(pdf).await;
    assert_eq!(body["gate"], "incomplete_run", "{body}");
    assert_eq!(body["ok"], false);
}

#[tokio::test]
async fn the_pdf_route_still_renders_a_clear_report() {
    if !typst_on_path() {
        eprintln!("pdf route clear test: skipping, typst is not on PATH");
        return;
    }
    let (state, id) = state_with(scan_report());
    let resp = pdf_route(state, &id).await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .map(|v| v.as_bytes()),
        Some(&b"application/pdf"[..])
    );
    use http_body_util::BodyExt;
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    assert!(bytes.starts_with(b"%PDF"), "a PDF body");
}

// ---- no route that writes a client-facing file can skip the gate --------------------------------

/// The production part of a source file: everything before its inline `#[cfg(test)]` module.
fn production_part(source: &str) -> &str {
    source
        .find("\n#[cfg(test)]\nmod ")
        .map_or(source, |at| &source[..at])
}

/// (`server/src/...` path, text) of every non-test source file of the server crate.
fn server_production_sources() -> Vec<(String, String)> {
    fn walk(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&src, &mut files);
    assert!(files.len() > 50, "the scan must see the server sources");
    files
        .into_iter()
        .filter_map(|file| {
            let name = file.to_string_lossy().to_string();
            if name.contains("/tests/") || name.ends_with("tests.rs") || name.contains("_tests") {
                return None;
            }
            let short = format!(
                "server/src/{}",
                file.strip_prefix(&src).ok()?.to_string_lossy()
            );
            Some((short, std::fs::read_to_string(&file).ok()?))
        })
        .collect()
}

/// The text of `async fn name(` through the next top-level item, from lib.rs's production part.
fn route_body<'a>(production: &'a str, name: &str) -> &'a str {
    let start = production
        .find(&format!("async fn {name}("))
        .unwrap_or_else(|| panic!("no route {name}"));
    let rest = &production[start..];
    let end = rest[1..]
        .find("\n}\n")
        .map_or(rest.len(), |at| at + 1 + "\n}\n".len());
    &rest[..end]
}

/// Every route that hands the caller a client-facing file (a download) and the gate call it must
/// make. A new route that writes such a file adds a row here and calls the gate; one that is not
/// listed fails the scan below.
const FILE_ROUTES: [(&str, &str); 2] = [
    ("export_audit_report", "gated_release("),
    ("export_product", "gated_release("),
];

#[test]
fn every_route_that_writes_a_client_file_calls_the_release_gate() {
    let lib = production_part(include_str!("../lib.rs"));

    // The scan: every function in the server crate's production code that sets a download
    // header or returns a PDF or zip body is a file route and must be in the table.
    let mut found = BTreeSet::new();
    for (path, source) in server_production_sources() {
        let production = production_part(&source);
        let mut current: Option<String> = None;
        for line in production.lines() {
            if let Some(name) = line
                .trim_start()
                .strip_prefix("async fn ")
                .or_else(|| line.trim_start().strip_prefix("pub async fn "))
                .or_else(|| line.trim_start().strip_prefix("pub(crate) async fn "))
            {
                current = name.split('(').next().map(str::to_string);
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                continue;
            }
            if line.contains("CONTENT_DISPOSITION")
                || line.contains("\"application/pdf\"")
                || line.contains("\"application/zip\"")
            {
                found.insert(format!(
                    "{path}::{}",
                    current.clone().unwrap_or_else(|| "(none)".to_string())
                ));
            }
        }
    }
    let expected: BTreeSet<String> = FILE_ROUTES
        .iter()
        .map(|(name, _)| format!("server/src/lib.rs::{name}"))
        .collect();
    assert_eq!(
        found, expected,
        "a route that writes a client file is not in FILE_ROUTES (or one listed no longer does)"
    );

    for (name, gate) in FILE_ROUTES {
        let body = route_body(lib, name);
        assert!(
            body.contains(gate),
            "{name} writes a client-facing file and must call {gate}"
        );
        // And neither route compiles a PDF or builds a zip on its own, around the gate.
        for bypass in [
            "compile_pdf_from_data(",
            "build_product_zip(",
            "compile_pdf(",
        ] {
            assert!(
                !body.contains(bypass),
                "{name} must not call {bypass} directly"
            );
        }
    }
}
