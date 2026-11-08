//! The standalone PDF route passes the same release gate as the package route (Fable wave 7,
//! round 2). `POST /audit-report` once ran only the secrets and unsynced checks, so a held row
//! (evidence unverified, a merge or a restore nobody decided, an Engine Check No nobody
//! corrected, a group decision missing a site, an incomplete run) did not stop a client PDF.
//!
//! Every held kind is run through both routes on the same state: the PDF route must refuse with
//! the same 422, naming the same sites, as the package route. A refusal needs no Typst: the held
//! checks run before the PDF is compiled, so these tests run with a Typst that does not exist and
//! never skip. Only a clear report, which compiles, needs it.

use std::collections::{BTreeSet, HashMap};

use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::review_view::{EngineCheckError, EngineCheckVerdictReq};
use serde_json::Value;

use super::tests::{production_part, scan_report};
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

/// An export request whose review screen still holds an undo for one site of the scan.
fn unsynced_request() -> axum::Json<crate::AuditReportReq> {
    use camerata_api_types::decisions::{UnsyncedChange, UnsyncedKind};
    let key = site_key(&scan_report(), "src/db.rs");
    axum::Json(crate::AuditReportReq {
        dispositions: HashMap::new(),
        options: crate::report_export::ReportOptions {
            unsynced_changes: vec![UnsyncedChange {
                key,
                change: UnsyncedKind::Undo,
            }],
            ..crate::report_export::ReportOptions::default()
        },
    })
}

/// An export request whose free-text options carry a key-shaped value, which reaches the PDF's
/// data without passing through the finding redactor.
fn secret_request() -> axum::Json<crate::AuditReportReq> {
    let planted =
        camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::StripeLiveSecretKey);
    axum::Json(crate::AuditReportReq {
        dispositions: HashMap::new(),
        options: crate::report_export::ReportOptions {
            prepared_by: format!("Prepared with {}", planted.value),
            ..crate::report_export::ReportOptions::default()
        },
    })
}

/// Run `fut` with a Typst that does not exist: a refusal must not need it, and a route that
/// reaches the compile answers 500 instead of 422.
async fn without_typst<F: std::future::Future>(fut: F) -> F::Output {
    crate::report_export::TYPST_PROGRAM
        .scope(std::path::PathBuf::from("/nonexistent/typst"), fut)
        .await
}

/// One held kind: the gate check that refuses it and how to build a state that holds it.
struct Held {
    name: &'static str,
    check: CheckId,
    build: fn() -> (crate::AppState, String),
    /// The export request that, with the state, is refused. Most held kinds live in the state and
    /// send the default request; the unsynced and secrets refusals live in the request.
    request: fn() -> axum::Json<crate::AuditReportReq>,
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

const HELD: [Held; 8] = [
    Held {
        name: "evidence unverified",
        check: CheckId::EvidenceUnverified,
        build: evidence_unverified,
        request,
    },
    Held {
        name: "junk content",
        check: CheckId::Content,
        build: junk_content,
        request,
    },
    Held {
        name: "merged without decision",
        check: CheckId::MergedWithoutDecision,
        build: merged_without_decision,
        request,
    },
    Held {
        name: "restored without decision",
        check: CheckId::RestoredWithoutDecision,
        build: restored_without_decision,
        request,
    },
    Held {
        name: "engine check no without correction",
        check: CheckId::EngineCheckNoWithoutCorrection,
        build: engine_check_no_without_correction,
        request,
    },
    Held {
        name: "node decision missing a member",
        check: CheckId::MergedWithoutDecision,
        build: node_decision_missing_a_member,
        request,
    },
    Held {
        name: "unsynced reviewer change",
        check: CheckId::UnsyncedReviewerChanges,
        build: || state_with(scan_report()),
        request: unsynced_request,
    },
    Held {
        name: "secret in the export options",
        check: CheckId::Secrets,
        build: || state_with(scan_report()),
        request: secret_request,
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

async fn pdf_route(
    state: crate::AppState,
    id: &str,
    req: axum::Json<crate::AuditReportReq>,
) -> axum::response::Response {
    crate::export_audit_report(
        axum::extract::State(state),
        axum::extract::Path(id.to_string()),
        req,
    )
    .await
}

async fn package_route(
    state: crate::AppState,
    id: &str,
    req: axum::Json<crate::AuditReportReq>,
) -> axum::response::Response {
    crate::export_product(
        axum::extract::State(state),
        axum::extract::Path(id.to_string()),
        req,
    )
    .await
}

/// The fixtures are real: the package route refuses each, naming the check that holds it.
#[tokio::test]
async fn each_held_fixture_is_refused_by_the_package_route() {
    for held in HELD {
        let (state, id) = (held.build)();
        let package = without_typst(package_route(state, &id, (held.request)())).await;
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
    let mut failures: Vec<String> = Vec::new();
    for held in HELD {
        let (state, id) = (held.build)();
        let package =
            body_json(without_typst(package_route(state.clone(), &id, (held.request)())).await)
                .await;

        let pdf = without_typst(pdf_route(state, &id, (held.request)())).await;
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

/// A held package that also leaks a secret is refused naming BOTH, on both routes, with no
/// Typst: the reviewer fixes everything in one pass, not one refusal at a time (Fable wave 12 F1).
/// The finding's path carries the secret, so it is in `findings.json` and every other file, and
/// the row also has an Engine Check No nobody corrected.
#[tokio::test]
async fn a_held_package_that_also_has_a_secret_is_refused_naming_both() {
    let planted =
        camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::StripeLiveSecretKey);
    let leaking_path = format!("src/{}.rs", planted.value);
    let mut report = scan_report();
    report.findings[0].path = leaking_path.clone();
    let key = site_key(&report, &leaking_path);
    let (state, id) = state_with(report);
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

    for route in ["package", "pdf"] {
        let resp = match route {
            "package" => without_typst(package_route(state.clone(), &id, request())).await,
            _ => without_typst(pdf_route(state.clone(), &id, request())).await,
        };
        assert_eq!(
            resp.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "{route}: refused"
        );
        let body = body_json(resp).await;
        let named = sites_named(&body);
        assert!(
            named
                .iter()
                .any(|(c, _, _)| c == CheckId::EngineCheckNoWithoutCorrection.label()),
            "{route}: names the Engine Check hold: {body}"
        );
        assert!(
            named.iter().any(|(c, _, _)| c == CheckId::Secrets.label()),
            "{route}: names the secret: {body}"
        );
        assert!(
            named
                .iter()
                .any(|(c, f, _)| c == CheckId::Secrets.label() && f == "findings.json"),
            "{route}: the secret is found in every package file, findings.json among them: {body}"
        );
    }
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
    let pdf = pdf_route(state, &id, request()).await;
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
    let resp = pdf_route(state, &id, request()).await;
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

/// What marks a line as writing a client file: a download header, or the content type of any file
/// the server can emit (PDF, zip, xlsx, CSV, JSON, Markdown, raw bytes). Compared lowercased.
const FILE_NEEDLES: [&str; 11] = [
    "content_disposition",
    "content-disposition",
    "application/pdf",
    "application/zip",
    "application/octet-stream",
    "spreadsheetml",
    "text/csv",
    "application/json",
    "text/markdown",
    "mime::",
    "text/plain",
];

/// The name of the function a line declares, in any form (`fn`, `async fn`, `pub fn`,
/// `pub(crate) async fn`, `const fn`, `unsafe fn`).
fn declared_fn(line: &str) -> Option<String> {
    let mut rest = line.trim_start();
    loop {
        let before = rest;
        for prefix in [
            "pub(crate) ",
            "pub(super) ",
            "pub ",
            "async ",
            "const ",
            "unsafe ",
        ] {
            rest = rest.strip_prefix(prefix).unwrap_or(rest);
        }
        if rest == before {
            break;
        }
    }
    let name = rest.strip_prefix("fn ")?;
    name.split(['(', '<']).next().map(str::to_string)
}

/// `path::fn` of every function in `source`'s production code, async or not, that sets a download
/// header or names the content type of a client file.
fn file_writing_fns(path: &str, source: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut current: Option<String> = None;
    for line in production_part(source).lines() {
        // Any function resets the attribution, so a helper written after a route is the helper's,
        // never the route's.
        if let Some(name) = declared_fn(line) {
            current = Some(name);
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if FILE_NEEDLES.iter().any(|needle| lower.contains(needle)) {
            found.insert(format!(
                "{path}::{}",
                current.clone().unwrap_or_else(|| "(none)".to_string())
            ));
        }
    }
    found
}

/// Every route that hands the caller a client-facing file (a download) and the gate call it must
/// make. A new route that writes such a file adds a row here and calls the gate; one that is not
/// listed fails the scan below.
const FILE_ROUTES: [(&str, &str); 2] = [
    ("export_audit_report", "gated_release("),
    ("export_product", "gated_release("),
];

/// Routes that write a file for the OPERATOR only: not reviewed, not released by the gate, never
/// a client deliverable. Each is named here so it cannot escape the scan, and each must say so in
/// its first line, its panel and its saved filename (below). `(route, saved filename)`.
///
/// `export_deep_report` is the only one: the deep-tier lenses are model-written prose with no
/// reviewer overlay and no gate. Making it a gated package artifact is a later work order; until
/// then it is labelled for what it is.
const OPERATOR_ONLY_ROUTES: [(&str, &str); 1] =
    [("export_deep_report", "camerata-deep-report-INTERNAL.md")];

/// What the operator-only deep report says on its first line, in the cockpit panel and on its
/// saved name.
const OPERATOR_ONLY_LABEL: &str = "Internal working notes, not reviewed, not for the client";

/// Functions the scan finds that are not routes serving the audit deliverable, each with why it
/// is not gated. `(site, reason)`.
const NOT_CLIENT_FILES: [(&str, &str); 5] = [
    (
        "server/src/api_agent_driver.rs::call_anthropic_with_tools",
        "an outbound request header to the model API, never a served file",
    ),
    (
        "server/src/api_agent_driver.rs::call_openrouter_with_tools",
        "an outbound request header to the model API, never a served file",
    ),
    (
        "server/src/client_source/handlers.rs::get_record_text",
        "a deletion record rendered from the store's own record (facts, no model-written prose), \
         not an audit report",
    ),
    (
        "server/src/routes/uow_routes.rs::default_attachment_mime_handler",
        "a serde default for an attachment's MIME field, never a served file",
    ),
    (
        "server/src/seed.rs::send",
        "the seed command's in-process request to the server's own routes (a JSON request \
         header), never a served file",
    ),
];

#[test]
fn every_route_that_writes_a_client_file_calls_the_release_gate() {
    let lib = production_part(include_str!("../lib.rs"));

    // The scan: every function in the server crate's production code that sets a download
    // header or returns a file body is a file route and must be in a table.
    let mut found = BTreeSet::new();
    for (path, source) in server_production_sources() {
        found.extend(file_writing_fns(&path, &source));
    }
    let expected: BTreeSet<String> = FILE_ROUTES
        .iter()
        .map(|(name, _)| format!("server/src/lib.rs::{name}"))
        .chain(
            OPERATOR_ONLY_ROUTES
                .iter()
                .map(|(name, _)| format!("server/src/lib.rs::{name}")),
        )
        .chain(NOT_CLIENT_FILES.iter().map(|(site, _)| (*site).to_string()))
        .collect();
    assert_eq!(
        found, expected,
        "a function that writes a file is in no table (FILE_ROUTES, OPERATOR_ONLY_ROUTES, \
         NOT_CLIENT_FILES), or one listed no longer does"
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

// ---- the scan itself is pinned: what it reads and what it recognises ---------------------------

#[test]
fn the_scan_reads_code_after_a_mod_tests_declaration() {
    // vuln_intel.rs declares `mod tests;` near its top and keeps nine async fns below it; a cut
    // at the first `#[cfg(test)] mod` would leave them unscanned.
    let sources = server_production_sources();
    let (_, source) = sources
        .iter()
        .find(|(path, _)| path == "server/src/vuln_intel.rs")
        .expect("vuln_intel.rs is scanned");
    let production = production_part(source);
    assert!(
        production.contains("async fn enrich_dependency_findings("),
        "the scan must read past the `mod tests;` declaration"
    );
}

#[test]
fn a_helper_after_a_route_is_not_attributed_to_the_route() {
    let source = "async fn export_product() {\n    gated_release();\n}\n\
                  fn zip_headers() {\n    let h = header::CONTENT_DISPOSITION;\n}\n";
    let found = file_writing_fns("p.rs", source);
    assert_eq!(
        found,
        BTreeSet::from(["p.rs::zip_headers".to_string()]),
        "a sync helper that sets a download header is itself a file-writing function"
    );
}

#[test]
fn every_client_file_type_the_server_can_emit_is_recognised() {
    let cases = [
        ("pdf", "\"application/pdf\""),
        ("zip", "\"application/zip\""),
        (
            "xlsx",
            "\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet\"",
        ),
        ("csv", "\"text/csv; charset=utf-8\""),
        ("json", "\"application/json\""),
        ("markdown", "\"text/markdown; charset=utf-8\""),
        ("octet-stream", "\"application/octet-stream\""),
        ("lowercase header", "\"content-disposition\""),
        ("mime crate", "mime::APPLICATION_PDF"),
    ];
    for (kind, needle) in cases {
        let source = format!("fn serve() {{\n    let t = {needle};\n}}\n");
        assert_eq!(
            file_writing_fns("p.rs", &source),
            BTreeSet::from(["p.rs::serve".to_string()]),
            "a {kind} body ({needle}) is a client file"
        );
    }
    let comment = "fn serve() {\n    // \"application/pdf\" is not served here\n}\n";
    assert!(file_writing_fns("p.rs", comment).is_empty());
}

// ---- the operator-only deep report (WO9e stop-gap) ----------------------------------------------

#[tokio::test]
async fn the_deep_report_says_it_is_operator_only_on_its_first_line_and_in_its_filename() {
    let state = crate::AppState::new(std::sync::Arc::new(crate::InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Acme", vec!["acme/portal".to_string()])
        .expect("project created");
    let mut report = crate::onboard::ScanReport::gated(&["acme/portal".to_string()]);
    report.deep = Some(crate::ai_audit::DeepReport {
        lenses: Vec::new(),
        advisory: true,
        disclaimer: "d".to_string(),
    });
    let job = state.jobs.create("audit", Some(project.id.clone()));
    state.jobs.finish(&job, report);

    let resp =
        crate::export_deep_report(axum::extract::State(state), axum::extract::Path(project.id))
            .await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let disposition = resp
        .headers()
        .get(axum::http::header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
        .unwrap_or_default();
    for (_, filename) in OPERATOR_ONLY_ROUTES {
        assert!(disposition.contains(filename), "{disposition}");
    }
    use http_body_util::BodyExt;
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    let markdown = String::from_utf8(bytes.to_vec()).expect("Markdown");
    let first = markdown.lines().next().unwrap_or_default();
    assert!(
        first.contains(OPERATOR_ONLY_LABEL),
        "the first line says who it is for: {first}"
    );
}

#[test]
fn the_cockpit_panel_and_saved_name_of_the_deep_report_say_it_is_operator_only() {
    let panel = include_str!("../../../ui/src/cockpit/scan/exports.rs");
    for (_, filename) in OPERATOR_ONLY_ROUTES {
        assert!(
            panel.contains(&format!("\"{filename}\"")),
            "the saved filename is {filename}"
        );
    }
    assert!(
        !panel.contains("\"camerata-deep-report.md\""),
        "no unlabelled saved name"
    );
    assert!(
        panel.contains(OPERATOR_ONLY_LABEL),
        "the panel carries the label"
    );
}
