#![allow(clippy::unwrap_used)]
//! Gate 2 of `docs/design/2026-10-20_machine-readable-exports.md` §Testing: for EVERY dataset
//! the product package ships, the CSV rows, the JSON rows and the xlsx worksheet rows agree.
//!
//! # What "agree" means here, per pair
//!
//! - **CSV vs JSON.** Same row count; the CSV header is exactly the union of the JSON rows'
//!   keys; and every CSV cell equals its JSON value under the documented flattening (string as
//!   is, number/bool as text, `null` or an absent key as empty, a nested array/object as
//!   compact JSON). The CSV is parsed with the `csv` crate, so a quoting bug (comma, quote,
//!   CR/LF inside a cell) shows up as a different row or column count.
//! - **JSON vs xlsx.** The workbook is read back cell by cell out of the real `.xlsx` bytes
//!   (no row struct is consulted, so this holds even if the writer and the JSON serializer
//!   diverge). Each worksheet column is projected from a JSON key by an explicit, independently
//!   written rule (a worksheet is not a dump of the JSON: it title-cases a severity, joins a
//!   location list with newlines, says "not counted" for an unknown denominator, and so on).
//!   Every JSON key must be either rendered by one of those columns or named in a per-dataset
//!   `json_only` allowlist with the reason, so a field added to the JSON and forgotten by the
//!   workbook (or vice versa) fails here instead of drifting.
//!
//! # Datasets and where each lives in the workbook
//!
//! The package ships EIGHT datasets (the design's "six" predates the Pass-3 worksheet
//! restructure that added `not-assessed` and `rules-applied`):
//!
//! | dataset | worksheet |
//! |---|---|
//! | `all-findings` | "All problems" (defect, deviation and kind-less rows only) |
//! | `deviations` | "Departures" |
//! | `conventions` | "Your conventions" |
//! | `not-assessed` | "Not checked", the rows whose kind of gap is "Rule could not be completed" |
//! | `dependencies` | "Dependencies" |
//! | `false-positives` | "Ruled out" |
//! | `coverage` | "Rules checked", the per-rule columns |
//! | `rules-applied` | "Rules checked", the Alternative applied and Evidence for it columns |
//!
//! `all-findings.json` is deliberately the UNPARTITIONED non-false-positive row set (see
//! `xlsx_export::FindingsExport`), so it is a superset of the "All problems" sheet; the rows
//! the workbook routes elsewhere by kind are accounted for against `conventions` and
//! `not-assessed` below, so every JSON row is still matched to exactly one worksheet row.
//!
//! The same assertions run with no corpus and with the real shipped corpus, because the corpus
//! changes titles, categories, fixes and which citations are "grounded".

use std::collections::{BTreeSet, HashMap};

use camerata_api_types::audit::{
    ConventionObservedKind, DeviationKind, FindingKind, NoConventionKind, NotAssessedKind,
    NotAssessedReason, ObservedOptionUsage,
};
use camerata_server::ai_audit::RuleRecommendation;
use camerata_server::onboard::{
    AuditedRef, Finding, LanguageVolume, MergedLocation, ScanProvenance, ScanReport, SealedReport,
};
use camerata_server::release_gate::{compare_csv_to_json, xlsx_sheet_grid};
use camerata_server::report_export::{finding_key, DispositionWire, ReportOptions};
use camerata_server::xlsx_export::{
    build_csv_exports, build_json_exports, build_workbook, DATASETS,
};
use serde_json::Value;

// ── Fixture ───────────────────────────────────────────────────────────────────────────────

fn finding(rule_id: &str, path: &str, line: usize, severity: &str) -> Finding {
    Finding {
        repo: "acme/portal".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: severity.to_string(),
        snippet: format!("snippet-{line}"),
        detail: format!("detail for {rule_id}"),
        ..Finding::default()
    }
}

fn kinded(mut f: Finding, kind: FindingKind) -> Finding {
    f.kind = Some(kind);
    f
}

fn deviation(sites: usize, assessed: Option<usize>, label: &str) -> FindingKind {
    FindingKind::Deviation(DeviationKind {
        deviating_sites: sites,
        assessed_sites: assessed,
        convention_option_id: Some("opt".to_string()),
        convention_label: label.to_string(),
    })
}

/// A report that populates ALL eight datasets, with values chosen to break a lazy
/// serializer: commas, double quotes, newlines, non-ASCII, nested lists, an unknown
/// denominator, a rule that came back clean, a dependency with and without a patch.
fn fixture() -> (SealedReport, HashMap<String, DispositionWire>) {
    let mut sql = kinded(
        finding("SEC-NO-RAW-SQL-CONCAT-1", "src/orders.rs", 10, "critical"),
        FindingKind::Defect,
    );
    sql.detail =
        "Raw SQL, built with format!(\"select {}\", id)\nsecond line: caf\u{e9}".to_string();
    sql.snippet = "let q = format!(\"select * from t where id = {}\", id);".to_string();
    sql.fix_specific = Some("Bind the id as a parameter,\nnever format it in.".to_string());
    sql.also_matches = vec!["ARCH-STRICT-LAYERING-1".to_string()];
    sql.also_locations = vec![
        MergedLocation::site(
            "acme/portal".to_string(),
            "src/refunds.rs".to_string(),
            22,
            "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
            "format!(...)".to_string(),
            false,
        ),
        MergedLocation::site(
            "acme/portal".to_string(),
            "src/report.rs".to_string(),
            3,
            "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
            "impact".to_string(),
            true,
        ),
    ];

    let mut in_test = kinded(
        finding("SEC-NO-HARDCODED-SECRETS-1", "tests/fixture.rs", 4, "low"),
        FindingKind::Defect,
    );
    in_test.in_test = true;
    in_test.needs_review = true;
    in_test.confidence = Some("needs-review".to_string());
    in_test.effort = Some("low".to_string());

    let mut preview = kinded(
        finding("semgrep.sql-injection", "src/legacy.rs", 7, "high"),
        FindingKind::Defect,
    );
    preview.preview = true;
    preview.preview_tool = Some("semgrep".to_string());

    let versioning_a = kinded(
        finding("ARCH-API-VERSIONING-1", "src/api/users.rs", 4, "medium"),
        deviation(2, Some(47), "URI-versioned endpoints"),
    );
    let versioning_b = kinded(
        finding("ARCH-API-VERSIONING-1", "src/api/teams.rs", 9, "medium"),
        deviation(2, Some(47), "URI-versioned endpoints"),
    );
    let logging = kinded(
        finding("ARCH-LOGGING-FORMAT-1", "src/log.rs", 2, "low"),
        deviation(1, None, "structured JSON logs"),
    );

    let convention = kinded(
        finding("ARCH-PAGINATION-1", "src/api/list.rs", 7, "info"),
        FindingKind::ConventionObserved(ConventionObservedKind {
            convention_option_id: "cursor".to_string(),
            convention_label: "Cursor pagination".to_string(),
        }),
    );
    let no_convention = kinded(
        finding("ARCH-ERROR-SHAPE-1", "src/api/errors.rs", 1, "info"),
        FindingKind::NoConvention(NoConventionKind {
            observed_options: vec![
                ObservedOptionUsage {
                    option_id: "rfc7807".to_string(),
                    label: "RFC 7807 problem+json".to_string(),
                    sites: Some(3),
                },
                ObservedOptionUsage {
                    option_id: "ad-hoc".to_string(),
                    label: "Ad-hoc error bodies".to_string(),
                    sites: None,
                },
            ],
        }),
    );

    let not_assessed_tool = kinded(
        finding("JS-NO-EVAL-1", "src/legacy.js", 2, "info"),
        FindingKind::NotAssessed(NotAssessedKind {
            reason: NotAssessedReason::ToolFailed,
            reason_detail: "eslint exited 2: spawn ENOENT".to_string(),
        }),
    );
    let not_assessed_pass = kinded(
        finding("PY-NO-EXEC-1", "src/job.py", 1, "info"),
        FindingKind::NotAssessed(NotAssessedKind {
            reason: NotAssessedReason::PassFailed,
            reason_detail: "pass timed out".to_string(),
        }),
    );

    let legacy = finding("LEGACY-UNMAPPED-1", "src/old.rs", 9, "high");

    let fp = kinded(
        finding("SEC-FP-1", "src/fixture.rs", 3, "high"),
        FindingKind::Defect,
    );
    let mut dispositions = HashMap::new();
    dispositions.insert(
        finding_key(&fp),
        DispositionWire {
            state: "FalsePositive".to_string(),
            reason: "test scaffolding, not shipped, \"safe\"".to_string(),
            bucket: String::new(),
            confirmed_by_client: false,
        },
    );

    let mut dep_patched = finding(
        camerata_server::dep_audit::DEP_AUDIT_RULE_ID,
        "Cargo.lock",
        0,
        "high",
    );
    dep_patched.snippet = "foo@1.0.0".to_string();
    dep_patched.detail = "GHSA-xxxx: foo, before 2.0.1 \"panics\"".to_string();
    dep_patched
        .captures
        .insert("osv_fixed_version".to_string(), "2.0.1".to_string());
    let mut dep_unpatched = finding(
        camerata_server::dep_audit::DEP_AUDIT_RULE_ID,
        "Cargo.lock",
        0,
        "critical",
    );
    dep_unpatched.snippet = "bar@0.1.0".to_string();
    dep_unpatched.detail = "GHSA-yyyy: bar has no patch yet".to_string();

    let findings = vec![
        sql,
        in_test,
        preview,
        versioning_a,
        versioning_b,
        logging,
        convention,
        no_convention,
        not_assessed_tool,
        not_assessed_pass,
        legacy,
        fp,
        dep_patched,
        dep_unpatched,
    ];

    let mut recommendations = HashMap::new();
    recommendations.insert(
        "ARCH-PAGINATION-1".to_string(),
        RuleRecommendation {
            repo: String::new(),
            rule_id: "ARCH-PAGINATION-1".to_string(),
            recommended_option_id: "cursor".to_string(),
            evidence: Some("src/api/list.rs:42 - cursor paging, with a comma".to_string()),
            applicable: true,
            ..RuleRecommendation::default()
        },
    );
    recommendations.insert(
        "ARCH-IDEMPOTENCY-1".to_string(),
        RuleRecommendation {
            repo: String::new(),
            rule_id: "ARCH-IDEMPOTENCY-1".to_string(),
            recommended_option_id: "none".to_string(),
            evidence: None,
            applicable: false,
            ..RuleRecommendation::default()
        },
    );

    let audited_rule_ids = [
        "SEC-NO-RAW-SQL-CONCAT-1",
        "ARCH-API-VERSIONING-1",
        "JS-NO-EVAL-1",
        "SEC-CLEAN-1",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    let report = ScanReport {
        run_id: None,
        tree: None,
        discarded: Vec::new(),
        repos_not_inspected: Vec::new(),
        recommendations_by_repo: Vec::new(),
        patterns_across_services: Default::default(),
        volume_by_repo: Default::default(),
        repos: vec!["acme/portal".to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 1,
        files_excluded: 0,
        excluded_noise_lines: None,
        code_chars: 5000,
        code_lines: 400,
        code_lines_by_language: vec![LanguageVolume {
            language: "Rust".to_string(),
            lines: 400,
        }],
        excluded_mechanical_rules: Vec::new(),
        findings,
        proposed_rules: Vec::new(),
        gated: false,
        blocked: false,
        ai_blocked_reason: None,
        ai_error: None,
        message: None,
        actual_usage: None,
        deep: None,
        coverage_notes: Vec::new(),
        provenance: ScanProvenance {
            group_review_model: None,
            audited_refs: vec![AuditedRef {
                repo: "acme/portal".to_string(),
                sha: Some("abcdef1234567890".to_string()),
                branch: Some("main".to_string()),
                dirty: false,
                ..AuditedRef::default()
            }],
            audit_model: Some("test-model".to_string()),
            calibration_model: None,
            clone_judge_model: None,
            mode: "parallel".to_string(),
            thorough: false,
            deep: false,
            rules_fingerprint: "fp".to_string(),
            audited_rule_ids,
            camerata_version: "0.0.0-test".to_string(),
            osv_scanner_version: Some("1.9.0".to_string()),
            vulnerability_data: None,
            started_at: "2026-10-21T00:00:00Z".to_string(),
            finished_at: "2026-10-21T00:05:00Z".to_string(),
            model_sessions: Vec::new(),
        },
        recommendations,
        failed_passes: Vec::new(),
        ledger: camerata_server::scan_ledger::ScanLedger::new(),
    };
    (report.seal(), dispositions)
}

fn empty_report() -> SealedReport {
    let (sealed, _) = fixture();
    let mut report = sealed.as_scan_report().clone();
    report.findings.clear();
    report.recommendations.clear();
    report.provenance.audited_rule_ids.clear();
    report.seal()
}

async fn real_corpus() -> Option<camerata_rules::RuleSet> {
    let path = std::path::Path::new(camerata_rules::DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(
        camerata_rules::load_corpus(path)
            .await
            .expect("the real corpus loads"),
    )
}

// ── A real xlsx reader (cells out of the .xlsx bytes) ─────────────────────────────────────

/// A worksheet as a dense grid of cell TEXT (`grid[row][col]`, blank cells as `""`), read out of
/// the real `.xlsx` bytes by the release gate's own reader (`release_gate::xlsx_sheet_grid`), so
/// the gate and this test cannot disagree about what a cell says.
fn read_sheet(xlsx: &[u8], sheet_name: &str) -> Vec<Vec<String>> {
    xlsx_sheet_grid(xlsx, sheet_name).unwrap_or_else(|e| panic!("{e}"))
}

// ── JSON / CSV readers and the shared comparison rules ────────────────────────────────────

fn rows_of(json_bytes: &[u8]) -> Vec<Value> {
    serde_json::from_slice::<Value>(json_bytes)
        .expect("dataset JSON parses")
        .as_array()
        .expect("dataset JSON is an array of rows")
        .clone()
}

/// A JSON value as one text cell: string as is, number/bool as text, `null`/absent as empty,
/// a nested array/object as compact JSON. This is the documented CSV flattening, restated here
/// independently of the production serializer.
fn text(row: &Value, key: &str) -> String {
    match row.get(key) {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(b)) => b.to_string(),
        Some(Value::Number(n)) => n.to_string(),
        Some(other) => serde_json::to_string(other).unwrap(),
    }
}

fn keys_of(rows: &[Value]) -> BTreeSet<String> {
    rows.iter()
        .flat_map(|r| r.as_object().expect("a row is an object").keys().cloned())
        .collect()
}

/// CSV against JSON for one dataset: the comparison is `release_gate::compare_csv_to_json` (row
/// count, header set, every cell), the same function the release gate runs on every package.
fn assert_csv_matches_json(dataset: &str, csv_bytes: &[u8], rows: &[Value]) {
    let json_bytes = serde_json::to_vec(rows).unwrap();
    let problems = compare_csv_to_json(dataset, csv_bytes, &json_bytes);
    assert!(problems.is_empty(), "{dataset}: {problems:?}");
}

/// Every key in the JSON rows is either rendered by a worksheet column (`rendered`) or named in
/// `json_only` (machine-readable only, with a reason at the call site), and every `rendered`
/// key really occurs: a stale mapping cannot silently pass.
fn assert_key_accounting(dataset: &str, rows: &[Value], rendered: &[&str], json_only: &[&str]) {
    let actual = keys_of(rows);
    let known: BTreeSet<String> = rendered
        .iter()
        .chain(json_only)
        .map(|s| s.to_string())
        .collect();
    let unaccounted: Vec<&String> = actual.difference(&known).collect();
    assert!(
        unaccounted.is_empty(),
        "{dataset}: JSON keys {unaccounted:?} are neither rendered in the worksheet nor listed \
         as JSON-only. Decide which, in this test."
    );
    let stale: Vec<&str> = rendered
        .iter()
        .copied()
        .filter(|k| !actual.contains(*k))
        .collect();
    assert!(
        stale.is_empty(),
        "{dataset}: the worksheet mapping names keys {stale:?} that no JSON row carries"
    );
}

/// Every row carries the identical key set (a JSON consumer can rely on the schema).
fn assert_uniform_keys(dataset: &str, rows: &[Value]) {
    let all = keys_of(rows);
    for (i, row) in rows.iter().enumerate() {
        let have: BTreeSet<String> = row.as_object().unwrap().keys().cloned().collect();
        assert_eq!(have, all, "{dataset}: row {i} has a different key set");
    }
}

fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    chars
        .next()
        .map(|f| f.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

fn count_text(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_u64)
        .unwrap_or_else(|| panic!("{key} is a non-negative integer in {row}"))
        .to_string()
}

// ── Per-dataset worksheet projections ─────────────────────────────────────────────────────

/// One worksheet column projected from one JSON key (or several): the header it sits under and
/// the text it must show for a JSON row.
type Projection = Vec<(&'static str, fn(&Value) -> String)>;

/// One projected column: a closure that captures nothing coerces to the function pointer here.
fn c(header: &'static str, f: fn(&Value) -> String) -> (&'static str, fn(&Value) -> String) {
    (header, f)
}

/// The data rows of a table sheet (header on its first row) as header-to-text maps. A row counts
/// only when `key_header` is filled: that keeps the one-line note an empty table carries from
/// being read as a row.
fn sheet_rows(xlsx: &[u8], sheet: &str, key_header: &str) -> Vec<HashMap<String, String>> {
    let grid = read_sheet(xlsx, sheet);
    let header = &grid[0];
    grid[1..]
        .iter()
        .map(|row| {
            header
                .iter()
                .enumerate()
                .map(|(c, h)| (h.clone(), row.get(c).cloned().unwrap_or_default()))
                .collect::<HashMap<_, _>>()
        })
        .filter(|row| row.get(key_header).is_some_and(|k| !k.is_empty()))
        .collect()
}

/// The JSON rows against the worksheet rows, column by column, in any order: each side is
/// projected to the same headers and the two sorted lists must be equal.
fn assert_sheet_matches(
    label: &str,
    sheet_rows: &[HashMap<String, String>],
    json_rows: &[&Value],
    projection: &Projection,
) {
    assert_eq!(
        sheet_rows.len(),
        json_rows.len(),
        "{label}: worksheet and JSON disagree on the row count"
    );
    let mut expected: Vec<Vec<String>> = json_rows
        .iter()
        .map(|r| projection.iter().map(|(_, f)| f(r)).collect())
        .collect();
    let mut actual: Vec<Vec<String>> = sheet_rows
        .iter()
        .map(|row| {
            projection
                .iter()
                .map(|(h, _)| {
                    row.get(*h)
                        .unwrap_or_else(|| panic!("{label}: the worksheet has no {h:?} column"))
                        .clone()
                })
                .collect()
        })
        .collect();
    expected.sort();
    actual.sort();
    assert_eq!(
        actual,
        expected,
        "{label}: worksheet cells differ from JSON (columns {:?})",
        projection.iter().map(|(h, _)| *h).collect::<Vec<_>>()
    );
}

fn yes_or_blank(row: &Value, key: &str) -> String {
    if row[key].as_bool().unwrap() {
        "Yes".to_string()
    } else {
        String::new()
    }
}

fn joined(row: &Value, key: &str, separator: &str) -> String {
    row[key]
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(separator)
        })
        .unwrap_or_default()
}

fn locations_text(row: &Value) -> String {
    row["also_locations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            let at = format!("{}:{}", l["path"].as_str().unwrap(), l["line"]);
            if l["consequence"].as_bool().unwrap() {
                format!("{at} (also affected)")
            } else {
                at
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_text(row: &Value) -> String {
    match text(row, "line").as_str() {
        "0" => "whole file".to_string(),
        n => n.to_string(),
    }
}

fn source_text(row: &Value) -> String {
    let provenance = text(row, "provenance");
    match provenance.strip_prefix("Preview: ") {
        Some(tool) => format!("Tool: {tool}"),
        None if provenance == "Deterministic" => provenance,
        None => "Model-reviewed".to_string(),
    }
}

/// The columns of All problems that carry one JSON key each. The other keys of a findings row
/// are accounted for in `FINDING_JSON_ONLY`.
fn problem_projection() -> Projection {
    vec![
        c("Severity", |r| title_case(&text(r, "severity"))),
        c("Problem", |r| text(r, "headline")),
        c("Repo", |r| text(r, "repo")),
        c("File", |r| text(r, "path")),
        c("Line", line_text),
        c("Area", |r| text(r, "category")),
        c("Effort", |r| text(r, "effort")),
        c("Estimated hours", |r| text(r, "est_hours")),
        c("Confidence", |r| match text(r, "confidence").as_str() {
            "needs-review" => "Low".to_string(),
            other => title_case(other),
        }),
        c("Needs a decision", |r| yes_or_blank(r, "needs_review")),
        c("Source", source_text),
        c("Standard cited", |r| text(r, "citation_label")),
        c("Standard links", |r| text(r, "citation_urls")),
        c("Status", |r| text(r, "status")),
        c("Code seen", |r| text(r, "snippet")),
        c("More detail", |r| text(r, "detail")),
        c("What to do here", |r| {
            let specific = text(r, "fix_specific");
            if specific.is_empty() {
                text(r, "fix")
            } else {
                specific
            }
        }),
        c("Kind", |r| text(r, "kind")),
        c("Places", |r| text(r, "site_count")),
        c("Also found at", locations_text),
        c("Why the severity changed", |r| {
            text(r, "severity_adjustments")
        }),
        c("Carried from", |r| text(r, "carried_from")),
        c("Fix type", |r| text(r, "fix_type")),
        c("Reviewer edited", |r| yes_or_blank(r, "reviewer_edited")),
        c("Edit ids", |r| joined(r, "edit_ids", ", ")),
        c("Place id", |r| text(r, "site_id")),
        c("Review level", |r| text(r, "review_level")),
        c("Fix id", |r| text(r, "fix_id")),
        c("Merged into", |r| text(r, "merged_into")),
    ]
}

/// Keys of a findings row the All problems worksheet shows in a derived form, not one-to-one:
/// `rule_id` and `also_matches` are in the Rules and Rule ids columns, `bucket` is the fix order,
/// `disposition_label` is the decision so far, `escalated` is internal-only.
const FINDING_DERIVED_KEYS: [&str; 5] = [
    "rule_id",
    "also_matches",
    "bucket",
    "disposition_label",
    "escalated",
];

/// Keys of a findings row that no All problems column carries: the machine-readable files are
/// the complete record.
/// - `in_test`, `sub_group`, `sub_group_cause`: the fix's label and cause are on Fix plan;
/// - `group`: the rule group's id, listed on How problems were grouped (the Fix id column names
///   the Fix, which is `fix_id`);
/// - `also_locations`: flattened into Also found at (checked above) and listed in full on All
///   source rows; `flagged_by`: the collapsed sources, listed on All source rows;
/// - `is_fp` / `fp_reason`: the Ruled out sheet's identity and reason;
/// - the kind payload (`deviating_sites` ... `reason_detail`): rendered on Departures, Your
///   conventions and Not checked, which this test checks against their own datasets.
const FINDING_JSON_ONLY: [&str; 17] = [
    "group",
    "in_test",
    "sub_group",
    "sub_group_cause",
    "also_locations",
    "flagged_by",
    "is_fp",
    "fp_reason",
    "deviating_sites",
    "assessed_sites",
    "convention_option_id",
    "convention_label",
    "observed_options",
    "reason",
    "reason_detail",
    "disposition_kind",
    "citation_kind",
];

/// Keys every findings-style row carries regardless of kind (everything except the flattened
/// kind payload).
const FINDING_BASE_KEYS: [&str; 41] = [
    "severity",
    "headline",
    "repo",
    "path",
    "line",
    "rule_id",
    "category",
    "bucket",
    "disposition_label",
    "effort",
    "est_hours",
    "confidence",
    "needs_review",
    "in_test",
    "provenance",
    "citation_label",
    "citation_urls",
    "also_matches",
    "status",
    "snippet",
    "detail",
    "fix_specific",
    "fix",
    "kind",
    "also_locations",
    "site_count",
    "severity_adjustments",
    "carried_from",
    "fix_type",
    "reviewer_edited",
    "edit_ids",
    "site_id",
    "review_level",
    "group",
    "fix_id",
    "sub_group",
    "sub_group_cause",
    "escalated",
    "merged_into",
    "is_fp",
    "fp_reason",
];

fn is_findings_sheet_kind(row: &Value) -> bool {
    matches!(
        row["kind"].as_str().unwrap(),
        "defect" | "deviation" | "unspecified"
    )
}

fn check_finding_datasets(
    xlsx: &[u8],
    all_findings: &[Value],
    false_positives: &[Value],
    conventions: &[Value],
    not_assessed: &[Value],
) {
    // JSON-side key discipline for the two FindingRow datasets.
    for (name, rows) in [
        ("all-findings", all_findings),
        ("false-positives", false_positives),
    ] {
        let rendered: Vec<&str> = PROBLEM_RENDERED_KEYS.to_vec();
        let mut json_only: Vec<&str> = FINDING_JSON_ONLY.to_vec();
        json_only.extend(FINDING_DERIVED_KEYS);
        assert_key_accounting(name, rows, &rendered, &json_only);
        let base: BTreeSet<&str> = FINDING_BASE_KEYS.into_iter().collect();
        for (i, row) in rows.iter().enumerate() {
            let have: BTreeSet<&str> = row
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect();
            let missing: Vec<&&str> = base.difference(&have).collect();
            assert!(
                missing.is_empty(),
                "{name}: row {i} lacks base keys {missing:?}"
            );
        }
    }

    // all-findings.json is the whole non-FP set; the workbook routes it by kind. Each class
    // must be matched against its own worksheet, with nothing left over.
    let on_findings_sheet: Vec<&Value> = all_findings
        .iter()
        .filter(|r| is_findings_sheet_kind(r))
        .collect();
    let convention_kinds = all_findings
        .iter()
        .filter(|r| {
            matches!(
                r["kind"].as_str().unwrap(),
                "convention_observed" | "no_convention"
            )
        })
        .count();
    let not_assessed_kinds = all_findings
        .iter()
        .filter(|r| r["kind"].as_str().unwrap() == "not_assessed")
        .count();
    assert_eq!(
        on_findings_sheet.len() + convention_kinds + not_assessed_kinds,
        all_findings.len(),
        "every all-findings row is a defect/deviation/unspecified, convention or not-assessed row"
    );
    assert_eq!(
        convention_kinds,
        conventions.len(),
        "conventions.json must have one row per convention-kind row of all-findings.json"
    );
    assert_eq!(
        not_assessed_kinds,
        not_assessed.len(),
        "not-assessed.json must have one row per not-assessed row of all-findings.json"
    );

    // All problems has one row for each place, whatever the number of rules there: the first row
    // at a place (repo, file, line) stands for it. A place with one row is held to every column; a
    // place with several is held to the columns the other rules do not add to, and the columns
    // they do add to are checked against what the rows say between them.
    let mut order: Vec<(String, String, String)> = Vec::new();
    let mut at_place: HashMap<(String, String, String), Vec<&Value>> = HashMap::new();
    for r in &on_findings_sheet {
        let key = (text(r, "repo"), text(r, "path"), text(r, "line"));
        at_place
            .entry(key.clone())
            .or_insert_with(|| {
                order.push(key.clone());
                Vec::new()
            })
            .push(r);
    }
    let all_sheet = sheet_rows(xlsx, "All problems", "Place id");
    assert_eq!(
        all_sheet.len(),
        order.len(),
        "All problems has one row for each place"
    );
    let single: Vec<&Value> = order
        .iter()
        .filter(|k| at_place[*k].len() == 1)
        .map(|k| at_place[k][0])
        .collect();
    let several: Vec<&Value> = order
        .iter()
        .filter(|k| at_place[*k].len() > 1)
        .map(|k| at_place[k][0])
        .collect();
    let ids_of =
        |rows: &[&Value]| -> BTreeSet<String> { rows.iter().map(|r| text(r, "site_id")).collect() };
    let (single_ids, several_ids) = (ids_of(&single), ids_of(&several));
    let sheet_of = |ids: &BTreeSet<String>| -> Vec<HashMap<String, String>> {
        all_sheet
            .iter()
            .filter(|row| ids.contains(&row["Place id"]))
            .cloned()
            .collect()
    };
    assert_sheet_matches(
        "All problems (places with one row)",
        &sheet_of(&single_ids),
        &single,
        &problem_projection(),
    );
    // What the other rules at a place add to: the cells that gather several rows.
    const GATHERED: [&str; 10] = [
        "Area",
        "Effort",
        "Estimated hours",
        "Needs a decision",
        "Standard cited",
        "Standard links",
        "What to do here",
        "Reviewer edited",
        "Edit ids",
        "Fix id",
    ];
    let steady: Projection = problem_projection()
        .into_iter()
        .filter(|(h, _)| !GATHERED.contains(h))
        .collect();
    assert_sheet_matches(
        "All problems (places with several rows)",
        &sheet_of(&several_ids),
        &several,
        &steady,
    );
    for row in sheet_of(&several_ids) {
        let members = order
            .iter()
            .map(|k| &at_place[k])
            .find(|m| text(m[0], "site_id") == row["Place id"])
            .expect("the place");
        let any = |key: &str| members.iter().any(|m| m[key].as_bool().unwrap());
        assert_eq!(row["Needs a decision"] == "Yes", any("needs_review"));
        assert_eq!(row["Reviewer edited"] == "Yes", any("reviewer_edited"));
        let areas: BTreeSet<String> = members.iter().map(|m| text(m, "category")).collect();
        let shown: BTreeSet<String> = row["Area"].split(", ").map(str::to_string).collect();
        assert_eq!(
            shown, areas,
            "All problems: Area lists every area at the place"
        );
        let fixes: BTreeSet<String> = members.iter().map(|m| text(m, "fix_id")).collect();
        let shown: BTreeSet<String> = row["Fix id"].split(", ").map(str::to_string).collect();
        assert_eq!(
            shown, fixes,
            "All problems: Fix id lists every Fix at the place"
        );
        let ids: BTreeSet<String> = row["Rule ids"].split(", ").map(str::to_string).collect();
        for m in members {
            assert!(
                ids.contains(&text(m, "rule_id")),
                "All problems: Rule ids lists every rule at the place"
            );
        }
    }
    // The rule columns: the Rules cell opens with the rule's title and lists the other rules;
    // the Rule ids cell opens with the row's own rule id.
    for row in sheet_rows(xlsx, "All problems", "Place id") {
        let json = on_findings_sheet
            .iter()
            .find(|r| r["site_id"].as_str() == Some(row["Place id"].as_str()))
            .expect("the worksheet row's place is in the JSON");
        assert_eq!(
            row["Rule ids"].split(", ").next().unwrap(),
            text(json, "rule_id"),
            "All problems: Rule ids opens with the row's own rule"
        );
        let also = text(json, "also_matches");
        if !also.is_empty() {
            let lists = if several_ids.contains(&row["Place id"]) {
                row["Rules"].contains(&also)
            } else {
                row["Rules"].ends_with(&also)
            };
            assert!(
                lists,
                "All problems: Rules lists the other rules {also:?}, got {:?}",
                row["Rules"]
            );
        }
    }

    // Ruled out: the reviewer's false positives, one row each.
    let ruled_out_projection: Projection = vec![
        c("Problem", |r| text(r, "headline")),
        c("File", |r| text(r, "path")),
        c("Line", line_text),
        c("Rule id", |r| text(r, "rule_id")),
        c("Reason", |r| text(r, "fp_reason")),
        c("Place id", |r| text(r, "site_id")),
    ];
    let fp_refs: Vec<&Value> = false_positives.iter().collect();
    assert_sheet_matches(
        "Ruled out",
        &sheet_rows(xlsx, "Ruled out", "Place id"),
        &fp_refs,
        &ruled_out_projection,
    );
}

/// Keys of a findings row that a column of All problems carries one-to-one (the projection
/// names the columns; this names the keys they come from).
const PROBLEM_RENDERED_KEYS: [&str; 29] = [
    "severity",
    "headline",
    "repo",
    "path",
    "line",
    "category",
    "effort",
    "est_hours",
    "confidence",
    "needs_review",
    "provenance",
    "citation_label",
    "citation_urls",
    "status",
    "snippet",
    "detail",
    "fix_specific",
    "fix",
    "kind",
    "site_count",
    "severity_adjustments",
    "carried_from",
    "fix_type",
    "reviewer_edited",
    "edit_ids",
    "site_id",
    "review_level",
    "fix_id",
    "merged_into",
];

fn check_deviations(xlsx: &[u8], rows: &[Value]) {
    assert_uniform_keys("deviations", rows);
    assert_key_accounting(
        "deviations",
        rows,
        &[
            "category",
            "rule_title",
            "convention_label",
            "deviating_sites",
            "assessed_sites",
            "worst_severity",
            "locations",
            "recommended_fix",
            "rule_id",
            "fix_type",
        ],
        &[],
    );
    let projection: Projection = vec![
        c("Area", |r| text(r, "category")),
        c("Rule", |r| text(r, "rule_title")),
        c("Convention", |r| text(r, "convention_label")),
        c("Places departing", |r| count_text(r, "deviating_sites")),
        c("Places checked", |r| {
            if r["assessed_sites"].is_null() {
                "not counted".to_string()
            } else {
                count_text(r, "assessed_sites")
            }
        }),
        c("Worst severity", |r| text(r, "worst_severity")),
        c("Where", |r| joined(r, "locations", "\n")),
        c("What to do", |r| text(r, "recommended_fix")),
        c("Rule id", |r| text(r, "rule_id")),
        c("Fix type", |r| text(r, "fix_type")),
    ];
    let refs: Vec<&Value> = rows.iter().collect();
    assert_sheet_matches(
        "Departures",
        &sheet_rows(xlsx, "Departures", "Rule id"),
        &refs,
        &projection,
    );
}

fn check_conventions(xlsx: &[u8], rows: &[Value]) {
    assert_uniform_keys("conventions", rows);
    assert_key_accounting(
        "conventions",
        rows,
        &[
            "category",
            "rule_title",
            "status",
            "convention_label",
            "patterns_in_use",
            "headline",
            "detail",
            "sites",
            "also_found_at",
            "rule_id",
        ],
        &[],
    );
    let projection: Projection = vec![
        c("Area", |r| text(r, "category")),
        c("What we checked", |r| text(r, "headline")),
        // What the code does: the convention followed, else the habits seen, else the row's own
        // statement.
        c("What your code does", |r| {
            let label = text(r, "convention_label");
            if !label.is_empty() {
                label
            } else if !text(r, "patterns_in_use").is_empty() {
                text(r, "patterns_in_use")
            } else {
                text(r, "headline")
            }
        }),
        c("How consistently", |r| text(r, "status")),
        c("Places looked at", |r| count_text(r, "sites")),
        c("Rule", |r| text(r, "rule_title")),
        c("More detail", |r| text(r, "detail")),
        c("Also found at", |r| text(r, "also_found_at")),
        c("Rule id", |r| text(r, "rule_id")),
    ];
    let refs: Vec<&Value> = rows.iter().collect();
    assert_sheet_matches(
        "Your conventions",
        &sheet_rows(xlsx, "Your conventions", "Rule id"),
        &refs,
        &projection,
    );
}

fn check_dependencies(xlsx: &[u8], rows: &[Value]) {
    assert_uniform_keys("dependencies", rows);
    // `ownership` is the machine tag behind the worksheet's "What to do" wording; the sheet
    // renders the consequence (the action), not the tag.
    assert_key_accounting(
        "dependencies",
        rows,
        &[
            "package",
            "advisory",
            "severity",
            "repo",
            "fixed_version",
            "action",
            "path",
            "site_id",
            "review_level",
            "fix_id",
            "reviewer_edited",
            "edit_ids",
        ],
        // The fix's label and cause, the escalation and the merge pointer are on the Fix plan,
        // How problems were grouped and All source rows sheets; the Dependencies worksheet has no
        // such column.
        &[
            "ownership",
            "group",
            "sub_group",
            "sub_group_cause",
            "escalated",
            "merged_into",
        ],
    );
    let projection: Projection = vec![
        c("Package", |r| text(r, "package")),
        c("Advisory", |r| text(r, "advisory")),
        c("Severity", |r| title_case(&text(r, "severity"))),
        c("Repo", |r| text(r, "repo")),
        c("Fixed in", |r| {
            r["fixed_version"]
                .as_str()
                .unwrap_or("(no patch yet)")
                .to_string()
        }),
        c("What to do", |r| {
            match (text(r, "action").as_str(), r["fixed_version"].as_str()) {
                ("", Some(v)) => format!("Upgrade to >= {v}."),
                (a, _) => a.to_string(),
            }
        }),
        c("File", |r| text(r, "path")),
        c("Place id", |r| text(r, "site_id")),
        c("Review level", |r| text(r, "review_level")),
        c("Fix id", |r| text(r, "fix_id")),
        c("Reviewer edited", |r| yes_or_blank(r, "reviewer_edited")),
        c("Edit ids", |r| joined(r, "edit_ids", ", ")),
    ];
    let refs: Vec<&Value> = rows.iter().collect();
    assert_sheet_matches(
        "Dependencies",
        &sheet_rows(xlsx, "Dependencies", "Place id"),
        &refs,
        &projection,
    );
}

fn check_not_assessed(xlsx: &[u8], rows: &[Value]) {
    assert_uniform_keys("not-assessed", rows);
    assert_key_accounting(
        "not-assessed",
        rows,
        &["rule_title", "why", "headline", "detail", "rule_id"],
        &[],
    );
    let projection: Projection = vec![
        c("Rule", |r| text(r, "rule_title")),
        c("Why", |r| text(r, "why")),
        c("What", |r| text(r, "headline")),
        c("More detail", |r| text(r, "detail")),
        c("Rule id", |r| text(r, "rule_id")),
    ];
    let refs: Vec<&Value> = rows.iter().collect();
    let on_sheet: Vec<HashMap<String, String>> = sheet_rows(xlsx, "Not checked", "Kind of gap")
        .into_iter()
        .filter(|r| r["Kind of gap"] == "Rule could not be completed")
        .collect();
    assert_sheet_matches("Not checked", &on_sheet, &refs, &projection);
}

fn check_coverage(
    xlsx: &[u8],
    rows: &[Value],
    all_findings: &[Value],
    conventions: &[Value],
    not_assessed: &[Value],
) {
    assert_uniform_keys("coverage", rows);
    // `citation_kind` is the machine tag behind the worksheet's "How we checked" wording.
    assert_key_accounting(
        "coverage",
        rows,
        &[
            "category",
            "title",
            "findings_count",
            "citation_kind",
            "citation_label",
            "rule_id",
        ],
        &[],
    );
    let projection: Projection = vec![
        c("Area", |r| text(r, "category")),
        c("Rule", |r| text(r, "title")),
        c("How we checked", |r| {
            match r["citation_kind"].as_str().unwrap() {
                "grounded" => "Deterministic",
                "preview" => "Tool",
                _ => "Model-reviewed",
            }
            .to_string()
        }),
        c("Standard cited", |r| text(r, "citation_label")),
        c("Rule id", |r| text(r, "rule_id")),
    ];
    let refs: Vec<&Value> = rows.iter().collect();
    // A rule that only has a chosen alternative has a row too (see rules-applied); it is not a
    // checked rule of the coverage dataset.
    let checked: Vec<HashMap<String, String>> = sheet_rows(xlsx, "Rules checked", "Rule id")
        .into_iter()
        .filter(|r| r["Result"] != "Alternative chosen")
        .collect();
    assert_sheet_matches("Rules checked", &checked, &refs, &projection);
    // The Result, derived from the datasets: clean is clean; a rule with a problem row reads
    // "Found n places"; a rule with no problem row whose rows are conventions reads "Convention
    // noted, no problem"; one whose rows are unfinished reads "Could not be completed".
    let rule_in = |set: &[Value], id: &str| set.iter().any(|r| text(r, "rule_id") == id);
    for r in rows {
        let id = text(r, "rule_id");
        let line = checked
            .iter()
            .find(|s| s["Rule id"] == id)
            .expect("the rule has its row");
        let n = r["findings_count"].as_u64().unwrap();
        let has_problem = all_findings
            .iter()
            .any(|f| is_findings_sheet_kind(f) && text(f, "rule_id") == id);
        let expected = if n == 0 {
            "Clean".to_string()
        } else if !has_problem && rule_in(conventions, &id) {
            "Convention noted, no problem".to_string()
        } else if !has_problem && rule_in(not_assessed, &id) {
            "Could not be completed".to_string()
        } else if n == 1 {
            "Found 1 place".to_string()
        } else {
            format!("Found {n} places")
        };
        assert_eq!(line["Result"], expected, "Result of {id}");
    }
}

fn check_rules_applied(xlsx: &[u8], rows: &[Value]) {
    assert_uniform_keys("rules-applied", rows);
    // `applicable` is the machine flag behind the not-applicable wording of the chosen cell.
    assert_key_accounting(
        "rules-applied",
        rows,
        &["rule_id", "chosen", "evidence"],
        &["applicable"],
    );
    let sheet = sheet_rows(xlsx, "Rules checked", "Rule id");
    for r in rows {
        let line = sheet
            .iter()
            .find(|s| s["Rule id"] == text(r, "rule_id"))
            .unwrap_or_else(|| panic!("Rules checked has no row for {}", text(r, "rule_id")));
        assert_eq!(
            line["Alternative applied"],
            text(r, "chosen"),
            "rules-applied: Alternative applied differs from JSON"
        );
        assert_eq!(
            line["Evidence for it"],
            text(r, "evidence"),
            "rules-applied: Evidence for it differs from JSON"
        );
    }
}

// ── The gate ──────────────────────────────────────────────────────────────────────────────

struct Artifacts {
    xlsx: Vec<u8>,
    json: HashMap<String, Vec<u8>>,
    csv: HashMap<String, Vec<u8>>,
}

fn build(
    report: &SealedReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
) -> Artifacts {
    let opts = ReportOptions::default();
    let by_dataset = |files: Vec<(String, Vec<u8>)>, dir: &str, ext: &str| {
        files
            .into_iter()
            .map(|(path, bytes)| {
                let name = path
                    .strip_prefix(&format!("{dir}/"))
                    .and_then(|p| p.strip_suffix(&format!(".{ext}")))
                    .unwrap_or_else(|| panic!("unexpected export path {path}"))
                    .to_string();
                (name, bytes)
            })
            .collect::<HashMap<_, _>>()
    };
    Artifacts {
        xlsx: build_workbook(
            &camerata_server::report_export::ReviewedReport::unreviewed(report, dispositions),
            corpus,
            &opts,
        )
        .unwrap(),
        json: by_dataset(
            build_json_exports(
                &camerata_server::report_export::ReviewedReport::unreviewed(report, dispositions),
                corpus,
                &opts,
            )
            .unwrap(),
            "json",
            "json",
        ),
        csv: by_dataset(
            build_csv_exports(
                &camerata_server::report_export::ReviewedReport::unreviewed(report, dispositions),
                corpus,
                &opts,
            )
            .unwrap(),
            "csv",
            "csv",
        ),
    }
}

fn assert_every_dataset_agrees(
    label: &str,
    report: &SealedReport,
    dispositions: &HashMap<String, DispositionWire>,
    corpus: Option<&camerata_rules::RuleSet>,
    expect_populated: bool,
) {
    let a = build(report, dispositions, corpus);

    // The dataset set is exactly the eight, in BOTH text formats. The CSV set also holds the
    // ticket file (WO9b), which has no JSON sibling.
    let expected: BTreeSet<String> = DATASETS.iter().map(|s| s.to_string()).collect();
    let mut expected_csv = expected.clone();
    expected_csv.insert("tickets".to_string());
    assert_eq!(a.json.keys().cloned().collect::<BTreeSet<_>>(), expected);
    assert_eq!(a.csv.keys().cloned().collect::<BTreeSet<_>>(), expected_csv);

    let mut rows: HashMap<&str, Vec<Value>> = HashMap::new();
    for dataset in DATASETS {
        let json_rows = rows_of(&a.json[dataset]);
        assert_csv_matches_json(&format!("{label}/{dataset}"), &a.csv[dataset], &json_rows);
        if expect_populated {
            assert!(
                !json_rows.is_empty(),
                "{label}/{dataset}: the fixture must populate every dataset or its parity \
                 check is vacuous"
            );
        } else {
            assert!(json_rows.is_empty(), "{label}/{dataset}: expected no rows");
        }
        rows.insert(dataset, json_rows);
    }

    check_finding_datasets(
        &a.xlsx,
        &rows["all-findings"],
        &rows["false-positives"],
        &rows["conventions"],
        &rows["not-assessed"],
    );
    check_deviations(&a.xlsx, &rows["deviations"]);
    check_conventions(&a.xlsx, &rows["conventions"]);
    check_dependencies(&a.xlsx, &rows["dependencies"]);
    check_not_assessed(&a.xlsx, &rows["not-assessed"]);
    check_coverage(
        &a.xlsx,
        &rows["coverage"],
        &rows["all-findings"],
        &rows["conventions"],
        &rows["not-assessed"],
    );
    check_rules_applied(&a.xlsx, &rows["rules-applied"]);
}

#[test]
fn gate2_csv_json_and_xlsx_agree_for_every_dataset_without_a_corpus() {
    let (report, dispositions) = fixture();
    assert_every_dataset_agrees("no-corpus", &report, &dispositions, None, true);
}

#[tokio::test]
async fn gate2_csv_json_and_xlsx_agree_for_every_dataset_with_the_real_corpus() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    let (report, dispositions) = fixture();
    assert_every_dataset_agrees("real-corpus", &report, &dispositions, Some(&corpus), true);
}

/// The degenerate case: a scan with nothing in it. All three formats must still agree that
/// every dataset is empty, and the workbook must not invent a data row.
#[test]
fn gate2_an_empty_scan_is_empty_in_every_format() {
    let report = empty_report();
    let a = build(&report, &HashMap::new(), None);
    for dataset in DATASETS {
        let rows = rows_of(&a.json[dataset]);
        assert!(rows.is_empty(), "{dataset}: {rows:?}");
        assert_csv_matches_json(dataset, &a.csv[dataset], &rows);
    }
    for (sheet, key) in [
        ("All problems", "Place id"),
        ("Dependencies", "Place id"),
        ("Ruled out", "Place id"),
    ] {
        let grid = read_sheet(&a.xlsx, sheet);
        assert!(
            !grid.is_empty() && grid[0].iter().any(|h| h == key),
            "{sheet} must keep its header row when empty"
        );
        let rows = sheet_rows(&a.xlsx, sheet, key);
        assert!(rows.is_empty(), "{sheet}: {rows:?}");
    }
}

/// The test's own reader and comparison rules must be able to FAIL; otherwise a green Gate 2
/// would only mean the reader returned something. Corrupt each artifact in turn.
#[test]
fn gate2_detects_a_disagreement_in_each_format() {
    let (report, dispositions) = fixture();
    let a = build(&report, &dispositions, None);
    let rows = rows_of(&a.json["conventions"]);

    // A CSV cell that no longer matches its JSON value.
    let tampered_csv = String::from_utf8(a.csv["conventions"].clone())
        .unwrap()
        .replacen("Cursor pagination", "Offset pagination", 1);
    assert!(std::panic::catch_unwind(|| {
        assert_csv_matches_json("tampered", tampered_csv.as_bytes(), &rows)
    })
    .is_err());

    // A CSV row dropped.
    let mut lines: Vec<&str> = std::str::from_utf8(&a.csv["conventions"])
        .unwrap()
        .lines()
        .collect();
    lines.pop();
    let short = lines.join("\n");
    assert!(std::panic::catch_unwind(|| {
        assert_csv_matches_json("short", short.as_bytes(), &rows)
    })
    .is_err());

    // A JSON row whose value no longer matches the worksheet.
    let mut drifted = rows.clone();
    drifted[0]["status"] = Value::String("Something else entirely".to_string());
    assert!(std::panic::catch_unwind(|| check_conventions(&a.xlsx, &drifted)).is_err());

    // A JSON key no worksheet column or allowlist entry accounts for.
    let mut extra = rows;
    extra[0]["brand_new_field"] = Value::String("x".to_string());
    assert!(std::panic::catch_unwind(|| check_conventions(&a.xlsx, &extra)).is_err());
}

/// CSV column order follows the JSON row struct's field order (the order the rows are declared
/// in), not alphabetical. That order comes from `serde_json`'s `preserve_order` feature, which
/// this crate now requests explicitly; before that it was enabled only by feature unification
/// with an unrelated crate, so removing that crate would have silently re-sorted every export's
/// columns. `rules-applied` is the smallest dataset whose declaration order is not alphabetical.
#[test]
fn gate2_csv_columns_follow_the_row_declaration_order() {
    let (report, dispositions) = fixture();
    let a = build(&report, &dispositions, None);
    let mut reader = csv::Reader::from_reader(a.csv["rules-applied"].as_slice());
    let headers: Vec<String> = reader.headers().unwrap().iter().map(String::from).collect();
    assert_eq!(
        headers,
        vec!["rule_id", "chosen", "evidence", "applicable"],
        "an alphabetical header means serde_json lost preserve_order"
    );
    let json_keys: Vec<String> = rows_of(&a.json["rules-applied"])[0]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(
        json_keys, headers,
        "JSON key order and CSV column order agree"
    );
}
