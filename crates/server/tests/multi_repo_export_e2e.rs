#![allow(clippy::unwrap_used)]
//! A project of several repos, through the export pipeline (report JSON, `findings.json`, the
//! workbook, the JSON and CSV datasets): the deliverable has to keep the services apart.
//!
//! * the same path in two repos is two places, so every site list names its repo;
//! * a rule violated in one service and clean in another is one row of "Patterns across
//!   services" and no finding, a rule violated in one and not applicable in another is no row,
//!   and the section is in no count;
//! * "What's healthy" says where a rule was clean and where it did not apply;
//! * the prose speaks of the services, and a one-repo project reads exactly as it always has.

mod common;

use std::collections::HashMap;

use camerata_server::onboard::ScanReport;
use camerata_server::release_gate::xlsx_sheet_grid;
use camerata_server::report_export::{
    build_report_json, AuditReportJson, ReportOptions, AUDIT_REPORT_DISCLAIMER,
    AUDIT_REPORT_DISCLAIMER_MULTI,
};
use camerata_server::scan_ledger::CellState;
use camerata_server::xlsx_export::{
    build_csv_exports, build_findings_export, build_json_exports, build_workbook,
};
use common::*;
use serde_json::Value;

fn report_json(report: &ScanReport) -> AuditReportJson {
    build_report_json(
        &report.clone().seal(),
        &HashMap::new(),
        None,
        &ReportOptions::default(),
    )
}

fn findings_json(report: &ScanReport) -> Value {
    let sealed = report.clone().seal();
    let json = build_report_json(&sealed, &HashMap::new(), None, &ReportOptions::default());
    let export = build_findings_export(&sealed, &HashMap::new(), None, &json, &HashMap::new());
    serde_json::to_value(export).unwrap()
}

fn workbook(report: &ScanReport) -> Vec<u8> {
    build_workbook(
        &report.clone().seal(),
        &HashMap::new(),
        None,
        &ReportOptions::default(),
    )
    .unwrap()
}

fn grid(xlsx: &[u8], sheet: &str) -> Vec<Vec<String>> {
    xlsx_sheet_grid(xlsx, sheet).unwrap()
}

/// The cell under `header` in the first row whose `key_header` cell equals `key`.
fn cell(grid: &[Vec<String>], header: &str, key_header: &str, key: &str) -> String {
    let head = &grid[0];
    let col = head.iter().position(|h| h == header).unwrap();
    let key_col = head.iter().position(|h| h == key_header).unwrap();
    grid.iter()
        .skip(1)
        .find(|row| row.get(key_col).is_some_and(|c| c == key))
        .and_then(|row| row.get(col).cloned())
        .unwrap_or_else(|| panic!("no row with {key_header} = {key:?}"))
}

fn dataset(report: &ScanReport, file: &str) -> Vec<Value> {
    let opts = ReportOptions::default();
    let (_, bytes) = build_json_exports(&report.clone().seal(), &HashMap::new(), None, &opts)
        .unwrap()
        .into_iter()
        .find(|(name, _)| name == file)
        .unwrap();
    serde_json::from_slice::<Value>(&bytes)
        .unwrap()
        .as_array()
        .unwrap()
        .clone()
}

// ── H6: the same path in two repos is two places ──────────────────────────────────────────

#[test]
fn two_repos_with_the_same_path_produce_distinct_sites_in_every_site_list() {
    let report = two_repo_report();
    let xlsx = workbook(&report);

    // "Also found at" on the findings sheet: the absorbed site names its repo.
    let all = grid(&xlsx, "All Findings");
    let head = &all[0];
    let (repo_col, file_col, also_col) = (
        head.iter().position(|h| h == "Repo").unwrap(),
        head.iter().position(|h| h == "File").unwrap(),
        head.iter().position(|h| h == "Also found at").unwrap(),
    );
    let sql_rows: Vec<&Vec<String>> = all
        .iter()
        .skip(1)
        .filter(|r| r[file_col] == "src/index.ts")
        .collect();
    assert_eq!(sql_rows.len(), 2, "one row per repo for the same path");
    let repos: Vec<&str> = sql_rows.iter().map(|r| r[repo_col].as_str()).collect();
    assert!(repos.contains(&WEB) && repos.contains(&API), "{repos:?}");
    let web_row = sql_rows.iter().find(|r| r[repo_col] == WEB).unwrap();
    assert!(
        web_row[also_col].contains("acme/web/src/other.ts:7"),
        "an absorbed site names its repo: {:?}",
        web_row[also_col]
    );

    // The Deviations sheet's "Where" and its service column.
    let dev = grid(&xlsx, "Deviations");
    assert_eq!(
        cell(&dev, "Where", "Rule ID", PAGINATION_RULE),
        "acme/web/src/list.ts:3"
    );
    assert_eq!(cell(&dev, "Service", "Rule ID", PAGINATION_RULE), WEB);
    assert_eq!(
        cell(
            &dev,
            "Convention this service follows",
            "Rule ID",
            PAGINATION_RULE
        ),
        "cursor pagination"
    );

    // The JSON and CSV datasets carry the repo too.
    let deviations = dataset(&report, "json/deviations.json");
    assert_eq!(deviations[0]["repo"], WEB);
    assert_eq!(deviations[0]["locations"][0], "acme/web/src/list.ts:3");
    let csv = build_csv_exports(
        &report.clone().seal(),
        &HashMap::new(),
        None,
        &ReportOptions::default(),
    )
    .unwrap();
    let (_, dev_csv) = csv.iter().find(|(n, _)| n == "csv/deviations.csv").unwrap();
    let dev_csv = String::from_utf8(dev_csv.clone()).unwrap();
    assert!(dev_csv.contains("acme/web/src/list.ts:3"), "{dev_csv}");
}

#[test]
fn the_pdf_sites_name_their_repo() {
    let json = report_json(&two_repo_report());
    let sql = json
        .curated_findings
        .iter()
        .find(|g| g.rule_id == SQL_RULE)
        .unwrap();
    let web = sql.sites.iter().find(|s| s.repo == WEB).unwrap();
    assert_eq!(web.also_sites[0].repo, WEB);
    assert_eq!(web.also_sites[0].path, "src/other.ts");
    assert!(sql
        .sites
        .iter()
        .any(|s| s.repo == API && s.path == "src/index.ts"));
}

// ── patterns across services ──────────────────────────────────────────────────────────────

#[test]
fn a_rule_violated_in_one_repo_and_clean_in_another_is_one_pattern_row_and_no_finding() {
    let report = two_repo_report();
    let json = report_json(&report);
    let patterns = json.patterns_across_services.as_ref().expect("a section");
    assert_eq!(patterns.repos, vec![WEB.to_string(), API.to_string()]);
    let ids: Vec<&str> = patterns.rows.iter().map(|r| r.rule_id.as_str()).collect();
    assert_eq!(ids, vec![PAGINATION_RULE], "only the violated/clean rule");
    let row = &patterns.rows[0];
    assert_eq!(
        row.statement,
        format!(
            "These services handle {PAGINATION_RULE} differently. If that is intended, record it \
             as a convention; if not, a shared library or a written standard would close the gap."
        )
    );
    let state = |repo: &str| {
        row.cells
            .iter()
            .find(|c| c.repo == repo)
            .unwrap()
            .state
            .clone()
    };
    assert_eq!(state(WEB), CellState::Violated { sites: 1 });
    assert_eq!(state(API), CellState::Clean);

    // findings.json: its own key, never inside the findings array.
    let fj = findings_json(&report);
    let section = &fj["patterns_across_services"];
    assert_eq!(section["rows"].as_array().unwrap().len(), 1);
    assert_eq!(section["rows"][0]["cells"][0]["state"], "violated");
    assert_eq!(section["rows"][0]["cells"][1]["state"], "clean");
    let findings = fj["findings"].as_array().unwrap();
    assert_eq!(
        findings.len(),
        report.findings.len(),
        "no finding was added"
    );
    assert!(
        findings
            .iter()
            .all(|f| f.get("statement").is_none() && f.get("cells").is_none()),
        "no pattern row is a finding"
    );
    // The pagination rule still has exactly its one real finding.
    assert_eq!(
        findings
            .iter()
            .filter(|f| f["rule_id"] == PAGINATION_RULE)
            .count(),
        1
    );
}

#[test]
fn a_rule_violated_in_one_repo_and_not_applicable_in_another_makes_no_row() {
    let json = report_json(&two_repo_report());
    let patterns = json.patterns_across_services.unwrap();
    assert!(
        !patterns.rows.iter().any(|r| r.rule_id == LOGGING_RULE),
        "violated in web and not applicable in api is not a pattern"
    );
    // The SQL rule is violated in both: no clean cell, no row.
    assert!(!patterns.rows.iter().any(|r| r.rule_id == SQL_RULE));
}

#[test]
fn the_section_is_in_no_severity_total_or_finding_count() {
    let report = two_repo_report();
    let json = report_json(&report);
    // Four real findings (one of them with an absorbed site): the cover counts exactly them.
    let stats = &json.cover.stats;
    assert_eq!(stats.high, 2);
    assert_eq!(stats.medium, 2);
    assert_eq!(stats.critical + stats.low, 0);
    // The pattern rows carry no severity at all.
    let fj = findings_json(&report);
    let row = serde_json::to_string(&fj["patterns_across_services"]["rows"][0]).unwrap();
    assert!(!row.contains("severity"), "{row}");
    // Removing the section changes no count, so the section was in none.
    let mut without = json.clone();
    without.patterns_across_services = None;
    assert_eq!(without.cover.stats.high, json.cover.stats.high);
    assert_eq!(
        without.executive_summary.narrative, json.executive_summary.narrative,
        "the narrative never reads the section"
    );
    // The workbook sheet exists, is a sheet of its own, and the Index totals are the findings'.
    let xlsx = workbook(&report);
    let sheet = grid(&xlsx, "Patterns across services");
    let flat: Vec<&str> = sheet.iter().flatten().map(String::as_str).collect();
    assert!(flat.iter().any(|c| c.starts_with("Violated 1")), "{flat:?}");
    assert!(flat.contains(&"Clean"), "{flat:?}");
    assert!(
        !sheet[3].iter().any(|h| h.eq_ignore_ascii_case("severity")),
        "the sheet has no severity column: {:?}",
        sheet[3]
    );
}

#[test]
fn the_pattern_sheet_shows_not_applicable_and_not_reviewed_as_themselves() {
    // A third service: the pagination rule was bound away from it, and the logging rule is
    // violated in web, checked clean in api and not reviewed in worker.
    let mut report = two_repo_report();
    report.repos.push("acme/worker".to_string());
    report
        .ledger
        .note_rule_scope(PAGINATION_RULE, &[WEB.to_string(), API.to_string()]);
    let json = report_json(&report);
    let patterns = json.patterns_across_services.unwrap();
    let row = patterns
        .rows
        .iter()
        .find(|r| r.rule_id == PAGINATION_RULE)
        .unwrap();
    let worker = row.cells.iter().find(|c| c.repo == "acme/worker").unwrap();
    assert!(
        matches!(worker.state, CellState::NotApplicable { .. }),
        "{:?}",
        worker.state
    );
    assert!(
        worker.label.starts_with("Not applicable"),
        "{}",
        worker.label
    );

    let xlsx = workbook(&report);
    let sheet = grid(&xlsx, "Patterns across services");
    let flat: Vec<&str> = sheet.iter().flatten().map(String::as_str).collect();
    assert!(
        flat.iter().any(|c| c.starts_with("Not applicable (")),
        "{flat:?}"
    );
}

#[test]
fn a_one_repo_report_has_no_section_no_sheet_and_no_key() {
    let report = one_repo_report();
    let json = report_json(&report);
    assert!(json.patterns_across_services.is_none());
    let fj = findings_json(&report);
    assert!(fj.get("patterns_across_services").is_none());
    let xlsx = workbook(&report);
    let names = camerata_server::release_gate::xlsx_sheet_names(&xlsx).unwrap();
    assert!(
        !names.iter().any(|n| n == "Patterns across services"),
        "{names:?}"
    );
}

// ── H5: per-repo conventions; wording ─────────────────────────────────────────────────────

#[test]
fn a_one_repo_report_reads_exactly_as_it_always_has() {
    let report = one_repo_report();
    let json = report_json(&report);
    assert_eq!(json.disclaimer, AUDIT_REPORT_DISCLAIMER);
    assert!(json
        .methodology
        .next_steps
        .contains("re-scans the repository once"));
    assert!(json
        .methodology
        .not_done
        .iter()
        .any(|l| l
            == "No runtime or dynamic analysis; findings derive from static repository state."));
    let healthy = json
        .whats_healthy
        .rules
        .iter()
        .find(|r| r.rule_id == HEALTHY_RULE)
        .unwrap();
    assert!(healthy.clean_in.is_empty() && healthy.not_applicable_in.is_empty());
    let cv = json.cover.code_volume.as_ref().unwrap();
    assert!(cv.per_repo.is_empty());

    let xlsx = workbook(&report);
    let dev = grid(&xlsx, "Deviations");
    assert!(dev[0]
        .iter()
        .any(|h| h == "Convention your codebase follows"));
    assert!(!dev[0].iter().any(|h| h == "Service"));
    // A site is a bare path and line.
    assert_eq!(
        cell(&dev, "Where", "Rule ID", PAGINATION_RULE),
        "src/list.ts:3"
    );
    let all = grid(&xlsx, "All Findings");
    assert!(
        cell(&all, "Also found at", "Rule ID", SQL_RULE).contains("src/other.ts:7")
            && !cell(&all, "Also found at", "Rule ID", SQL_RULE).contains("acme/web/"),
    );
    let cov = grid(&xlsx, "Coverage");
    assert!(cov[1][0].contains("against this codebase"), "{:?}", cov[1]);
    let deviations = dataset(&report, "json/deviations.json");
    assert!(deviations[0].get("repo").is_none());
}

#[test]
fn a_multi_repo_report_speaks_of_the_services() {
    let report = two_repo_report();
    let json = report_json(&report);
    assert_eq!(json.disclaimer, AUDIT_REPORT_DISCLAIMER_MULTI);
    assert!(!json.disclaimer.contains("the repository state identified"));
    assert!(json
        .methodology
        .next_steps
        .contains("re-scans the repositories once"));
    assert!(!json.methodology.next_steps.contains("the repository once"));

    let xlsx = workbook(&report);
    let cov = grid(&xlsx, "Coverage");
    assert!(cov[1][0].contains("against these services"), "{:?}", cov[1]);
    assert!(!cov[1][0].contains("this codebase"));

    // The cover's code volume says how large each repo is.
    let cv = json.cover.code_volume.as_ref().unwrap();
    assert_eq!(cv.lines, 400);
    assert_eq!(
        cv.per_repo
            .iter()
            .map(|r| (r.repo.as_str(), r.lines))
            .collect::<Vec<_>>(),
        vec![(WEB, 300), (API, 100)]
    );
}

// ── What's healthy names the repos ────────────────────────────────────────────────────────

#[test]
fn whats_healthy_names_where_a_rule_was_clean_and_where_it_did_not_apply() {
    let json = report_json(&two_repo_report());
    let healthy = json
        .whats_healthy
        .rules
        .iter()
        .find(|r| r.rule_id == HEALTHY_RULE)
        .expect("the rule is healthy where it applies");
    assert_eq!(healthy.clean_in, vec![WEB.to_string()]);
    assert_eq!(healthy.not_applicable_in, vec![API.to_string()]);
}

#[test]
fn the_coverage_sheet_does_not_call_a_rule_clean_that_only_one_service_checked() {
    let xlsx = workbook(&two_repo_report());
    let cov = grid(&xlsx, "Coverage");
    // The table starts on the fourth row, under the one-line summary.
    let result = cell(&cov[3..], "Result", "Rule ID", HEALTHY_RULE);
    assert!(
        result.starts_with("Clean where it applies"),
        "a rule clean in web and not applicable in api says where it is clean: {result:?}"
    );
    assert!(result.contains("acme/web: Clean"), "{result:?}");
    assert!(result.contains("acme/api: Not applicable"), "{result:?}");
}

// ── The PDF ───────────────────────────────────────────────────────────────────────────────

/// The compiled PDF carries the section after the findings, in the fixed wording, with the
/// services named and no severity beside it; a one-repo PDF has no such section.
#[tokio::test]
async fn the_pdf_has_the_patterns_section_for_several_repos_and_not_for_one() {
    if !camerata_test_env::typst_on_path() {
        eprintln!("skipping: no typst");
        return;
    }
    let text_of = |pdf: &[u8]| -> String {
        let text = pdf_extract::extract_text_from_mem(pdf).expect("text extracts");
        text.chars().filter(|c| *c != '\n').collect()
    };
    let opts = ReportOptions {
        client_name: "Example Client Ltd".to_string(),
        ..ReportOptions::default()
    };

    let multi = build_report_json(&two_repo_report().seal(), &HashMap::new(), None, &opts);
    let pdf = camerata_server::report_export::compile_pdf(&multi)
        .await
        .expect("compiles");
    let text = text_of(&pdf);
    assert!(text.contains("Patterns across services"), "{text}");
    assert!(text.contains("handle"), "the row sentence is printed");
    assert!(text.contains("acme/api"), "the services are named");
    assert!(
        text.contains("Clean in acme/web; not applicable in acme/api"),
        "What's healthy names where the rule applied: {text}"
    );
    // The section comes after the findings and before the healthy list.
    let at = |needle: &str| text.find(needle).unwrap_or_else(|| panic!("{needle}"));
    assert!(at("Held for review") < at("Patterns across services"));
    assert!(at("Patterns across services") < at("Clean in acme/web"));

    let single = build_report_json(&one_repo_report().seal(), &HashMap::new(), None, &opts);
    let pdf = camerata_server::report_export::compile_pdf(&single)
        .await
        .expect("compiles");
    assert!(!text_of(&pdf).contains("Patterns across services"));
}

// ── M7: the package name ──────────────────────────────────────────────────────────────────

#[test]
fn the_package_is_named_for_the_project_and_its_repo_count_not_for_its_first_repo() {
    let report = two_repo_report();
    assert_eq!(
        camerata_server::report_filename_stem_for(&report, "Acme Shop"),
        "camerata-inspection-acme-shop-2-repos"
    );
    // No project title: the owner the repos share.
    assert_eq!(
        camerata_server::report_filename_stem_for(&report, ""),
        "camerata-inspection-acme-2-repos"
    );
    // One repo keeps the name it always had: the repo and its commit.
    assert_eq!(
        camerata_server::report_filename_stem(&one_repo_report()),
        "camerata-inspection-acme-web-abcdef1"
    );
}

// ── the same credential in two repos ──────────────────────────────────────────────────────

fn secret_in(repo: &str, name: &str) -> camerata_server::onboard::Finding {
    camerata_server::onboard::Finding {
        repo: repo.to_string(),
        path: ".env".to_string(),
        line: 1,
        rule_id: "SEC-NO-HARDCODED-SECRETS-1".to_string(),
        severity: "high".to_string(),
        snippet: format!("{name}=[redacted]"),
        detail: format!("{name} is committed"),
        kind: Some(camerata_api_types::audit::FindingKind::Defect),
        ..camerata_server::onboard::Finding::default()
    }
}

#[test]
fn the_same_credential_in_two_repos_stays_two_rows_each_noting_the_other() {
    let mut report = two_repo_report();
    report.findings.push(secret_in(WEB, "STRIPE_SECRET_KEY"));
    report.findings.push(secret_in(API, "STRIPE_SECRET_KEY"));
    let before = report.findings.len();

    let fj = findings_json(&report);
    let rows: Vec<&Value> = fj["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule_id"] == "SEC-NO-HARDCODED-SECRETS-1")
        .collect();
    assert_eq!(rows.len(), 2, "each service's owner rotates their own");
    assert_eq!(fj["findings"].as_array().unwrap().len(), before);
    let web = rows.iter().find(|r| r["repo"] == WEB).unwrap();
    let api = rows.iter().find(|r| r["repo"] == API).unwrap();
    assert_eq!(
        web["same_credential_note"],
        "Same credential name also flagged in acme/api."
    );
    assert_eq!(
        api["same_credential_note"],
        "Same credential name also flagged in acme/web."
    );
    // Rows that are not about a credential carry no such key.
    assert!(fj["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule_id"] == SQL_RULE)
        .all(|f| f.get("same_credential_note").is_none()));

    // The workbook shows it in "Also found at"; the PDF's data carries it on the site.
    let xlsx = workbook(&report);
    let all = grid(&xlsx, "All Findings");
    let head = &all[0];
    let (repo_col, rule_col, also_col) = (
        head.iter().position(|h| h == "Repo").unwrap(),
        head.iter().position(|h| h == "Rule ID").unwrap(),
        head.iter().position(|h| h == "Also found at").unwrap(),
    );
    let web_row = all
        .iter()
        .skip(1)
        .find(|r| r[rule_col] == "SEC-NO-HARDCODED-SECRETS-1" && r[repo_col] == WEB)
        .unwrap();
    assert!(web_row[also_col].contains("also flagged in acme/api"));
    let json = report_json(&report);
    let group = json
        .curated_findings
        .iter()
        .chain(json.held_for_review_findings.iter())
        .find(|g| g.rule_id == "SEC-NO-HARDCODED-SECRETS-1")
        .unwrap();
    assert_eq!(group.sites.len(), 2, "two sites, never merged");
    assert!(group.sites.iter().all(|s| s
        .same_credential_note
        .starts_with("Same credential name also flagged in")));
}

#[test]
fn a_different_credential_name_in_another_repo_gets_no_note() {
    let mut report = two_repo_report();
    report.findings.push(secret_in(WEB, "STRIPE_SECRET_KEY"));
    report.findings.push(secret_in(API, "TWILIO_AUTH_TOKEN"));
    let fj = findings_json(&report);
    assert!(fj["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule_id"] == "SEC-NO-HARDCODED-SECRETS-1")
        .all(|f| f.get("same_credential_note").is_none()));
}

// ── H5: a row the evidence gate adds belongs to the repo whose evidence it is ─────────────

fn recommendation(repo: &str, evidence: &str) -> camerata_server::ai_audit::RuleRecommendation {
    camerata_server::ai_audit::RuleRecommendation {
        repo: repo.to_string(),
        rule_id: HEALTHY_RULE.to_string(),
        recommended_option_id: "some-option".to_string(),
        recommendation_reasoning: "the model's pick".to_string(),
        evidence: Some(evidence.to_string()),
        applicable: true,
        ..Default::default()
    }
}

#[test]
fn an_evidence_negation_row_is_attributed_to_the_repo_it_came_from_not_the_first_repo() {
    // The rule is healthy in both repos (ran, nothing found), and the evidence the model cited
    // for the SECOND repo negates the rule's own requirement.
    let mut report = two_repo_report();
    report.ledger = camerata_server::scan_ledger::ScanLedger::new();
    for repo in [WEB, API] {
        report.ledger.record_rule(
            repo,
            HEALTHY_RULE,
            camerata_server::scan_ledger::RuleTier::Semantic,
            true,
            None,
            5,
            0,
        );
    }
    report.findings.clear();
    report.recommendations_by_repo = vec![
        recommendation(
            WEB,
            "src/web/orders.ts:10 - opens a transaction around the writes",
        ),
        recommendation(
            API,
            "src/api/orders.ts:42 - three writes with no transaction",
        ),
    ];
    camerata_server::report_export::apply_evidence_gate(&mut report);
    assert_eq!(
        report.findings.len(),
        1,
        "only the negating evidence is flagged"
    );
    let row = &report.findings[0];
    assert_eq!(row.repo, API, "attributed to the repo whose evidence it is");
    assert_eq!(row.path, "src/api/orders.ts");
    assert_eq!(row.line, 42);
}

#[test]
fn a_single_repo_recommendation_map_still_attributes_to_the_first_repo() {
    // An older report keeps only the by-rule map; its rows read as they always did.
    let mut report = one_repo_report();
    report.ledger = camerata_server::scan_ledger::ScanLedger::new();
    report.ledger.record_rule(
        WEB,
        HEALTHY_RULE,
        camerata_server::scan_ledger::RuleTier::Semantic,
        true,
        None,
        5,
        0,
    );
    report.findings.clear();
    report.recommendations.insert(
        HEALTHY_RULE.to_string(),
        recommendation("", "src/orders.ts:42 - three writes with no transaction"),
    );
    camerata_server::report_export::apply_evidence_gate(&mut report);
    assert_eq!(report.findings.len(), 1);
    assert_eq!(report.findings[0].repo, WEB);
}

// ── README ────────────────────────────────────────────────────────────────────────────────

#[test]
fn the_readme_names_the_patterns_section_and_the_plural_disclaimer_only_for_several_repos() {
    use camerata_server::{product_export_readme, ManifestProvenance, ProvenanceRepo};
    let repo = |name: &str| ProvenanceRepo {
        name: name.to_string(),
        ..ProvenanceRepo::default()
    };
    let multi = ManifestProvenance {
        repos: vec![repo(WEB), repo(API)],
        ..ManifestProvenance::default()
    };
    let one = ManifestProvenance {
        repos: vec![repo(WEB)],
        ..ManifestProvenance::default()
    };
    let readme = product_export_readme("acme", &multi);
    assert!(readme.contains("Patterns across services"), "{readme}");
    assert!(
        readme.contains("the repository states identified"),
        "{readme}"
    );
    assert!(
        !readme.contains('\u{2014}'),
        "no em dash in buyer-facing text"
    );
    let readme = product_export_readme("acme", &one);
    assert!(!readme.contains("Patterns across services"), "{readme}");
    assert!(
        readme.contains("the repository state identified"),
        "{readme}"
    );
}

/// A row the reviewer dismissed as a false positive is not in the PDF, so no other row may point
/// at it: the note names only the repos whose rows the report still carries.
#[test]
fn a_credential_note_never_cites_a_row_dismissed_as_a_false_positive() {
    use camerata_server::report_export::{finding_key, DispositionWire};
    let mut report = two_repo_report();
    report.findings.push(secret_in(WEB, "STRIPE_SECRET_KEY"));
    let api_row = secret_in(API, "STRIPE_SECRET_KEY");
    let mut dispositions = HashMap::new();
    dispositions.insert(
        finding_key(&api_row),
        DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "fixture token".to_string(),
            ..DispositionWire::default()
        },
    );
    report.findings.push(api_row);
    let json = build_report_json(
        &report.seal(),
        &dispositions,
        None,
        &ReportOptions::default(),
    );
    let group = json
        .curated_findings
        .iter()
        .chain(json.held_for_review_findings.iter())
        .find(|g| g.rule_id == "SEC-NO-HARDCODED-SECRETS-1")
        .unwrap();
    assert_eq!(
        group.sites.len(),
        1,
        "the dismissed row is out of the report"
    );
    assert_eq!(
        group.sites[0].same_credential_note, "",
        "the note must not point at a row the report does not carry"
    );
}

/// The workbook Index lists every sheet with its number of findings. The patterns sheet holds
/// observations, not findings, so its row carries no count under that header.
#[test]
fn the_index_does_not_count_pattern_rows_as_findings() {
    let xlsx = workbook(&two_repo_report());
    let index = grid(&xlsx, "Index");
    let toc = index
        .iter()
        .position(|row| {
            row.first().is_some_and(|c| c == "Sheet") && row.get(1).is_some_and(|c| c == "Findings")
        })
        .expect("the Index has a table of contents");
    let row = index[toc + 1..]
        .iter()
        .find(|row| row.first().is_some_and(|c| c == "Patterns across services"))
        .expect("the patterns sheet is listed");
    assert_eq!(
        row.get(1).map(String::as_str).unwrap_or(""),
        "",
        "pattern rows are not findings: {row:?}"
    );
}

/// The shipped `findings.json` of a one-repo project is byte for byte what it was: the per-repo
/// volume key exists only when there is more than one repo to list.
#[test]
fn a_one_repo_findings_json_carries_no_per_repo_volume_key() {
    let one = findings_json(&one_repo_report());
    assert!(
        one["provenance"]["code_volume"].get("per_repo").is_none(),
        "{}",
        one["provenance"]["code_volume"]
    );
    let two = findings_json(&two_repo_report());
    assert_eq!(
        two["provenance"]["code_volume"]["per_repo"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
