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
//! | `all-findings` | "All Findings" (defect, deviation and kind-less rows only) |
//! | `deviations` | "Deviations" |
//! | `conventions` | "Conventions" |
//! | `not-assessed` | "Coverage", section "Checks we could not complete this scan" |
//! | `dependencies` | "Dependencies" |
//! | `false-positives` | "False Positives" |
//! | `coverage` | "Coverage", the main per-rule table |
//! | `rules-applied` | "Coverage", section "Rules applied (alternative selection)" |
//!
//! `all-findings.json` is deliberately the UNPARTITIONED non-false-positive row set (see
//! `xlsx_export::FindingsExport`), so it is a superset of the "All Findings" sheet; the rows
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

/// The data rows of the table whose header row is exactly `headers`: the rows after it up to
/// the first fully blank one, each truncated to `headers.len()` columns. `None` when no such
/// header row exists (so a missing section is distinguishable from an empty one).
fn table(grid: &[Vec<String>], headers: &[&str]) -> Option<Vec<Vec<String>>> {
    let width = headers.len();
    let cell = |row: &Vec<String>, c: usize| row.get(c).cloned().unwrap_or_default();
    let header_at = grid
        .iter()
        .position(|row| (0..width).all(|c| cell(row, c) == headers[c]))?;
    let mut rows = Vec::new();
    for row in &grid[header_at + 1..] {
        if row.iter().all(String::is_empty) {
            break;
        }
        rows.push((0..width).map(|c| cell(row, c)).collect());
    }
    Some(rows)
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

const FINDING_HEADERS: [&str; 28] = [
    "Severity",
    "Headline",
    "Repo",
    "File",
    "Line",
    "Rule ID",
    "Category",
    "Action",
    "Disposition",
    "Effort",
    "Est. hours",
    "Confidence",
    "Needs review",
    "In test scope",
    "Provenance",
    "Citation",
    "Citation URLs",
    "Also matches",
    "Status",
    "Snippet",
    "Detail",
    "Fix (specific to this finding)",
    "Recommended Fix",
    "Kind",
    "Sites",
    "Also found at",
    "Severity adjustments",
    "Carried from",
];

const FINDING_RENDERED: [&str; 26] = [
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
    "severity_adjustments",
    "carried_from",
];

/// Keys of a findings-style row that no findings-sheet COLUMN carries one-to-one:
/// - `also_locations`: rendered, but flattened into the "Also found at" column;
/// - `site_count`: rendered as the "Sites" column;
/// - `is_fp` / `fp_reason`: the False Positives sheet's identity and its extra "FP Reason"
///   column; the live sheets have no such column;
/// - the kind payload (`deviating_sites` ... `reason_detail`): the sheet shows the kind TAG only
///   in a findings row; the payload is rendered on the Deviations / Conventions / Coverage
///   tables, which this test checks against their own datasets.
const FINDING_EXTRA_KEYS: [&str; 12] = [
    "also_locations",
    "site_count",
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
];

/// Keys every findings-style row carries regardless of kind (everything except the flattened
/// kind payload).
const FINDING_BASE_KEYS: [&str; 30] = [
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
    "is_fp",
    "fp_reason",
];

fn bucket_title(bucket: &str) -> &'static str {
    match bucket {
        "do_now" => "Do now",
        "do_next" => "Do next",
        "plan" => "Plan",
        "informational" => "Informational",
        "held" => "Held for review",
        _ => "Accepted",
    }
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

fn yes_or_blank(row: &Value, key: &str) -> String {
    if row[key].as_bool().unwrap() {
        "Yes".to_string()
    } else {
        String::new()
    }
}

/// The 28 cells a findings-style worksheet row must show for `row`; a False Positives row adds
/// the "FP Reason" cell (and shows a blank Action, since a false positive has no matrix cell).
fn finding_cells(row: &Value, false_positive: bool) -> Vec<String> {
    let mut cells = vec![
        title_case(&text(row, "severity")),
        text(row, "headline"),
        text(row, "repo"),
        text(row, "path"),
        text(row, "line"),
        text(row, "rule_id"),
        text(row, "category"),
        if false_positive {
            String::new()
        } else {
            bucket_title(row["bucket"].as_str().unwrap()).to_string()
        },
        text(row, "disposition_label"),
        text(row, "effort"),
        text(row, "est_hours"),
        text(row, "confidence"),
        yes_or_blank(row, "needs_review"),
        yes_or_blank(row, "in_test"),
        text(row, "provenance"),
        text(row, "citation_label"),
        text(row, "citation_urls"),
        text(row, "also_matches"),
        text(row, "status"),
        text(row, "snippet"),
        text(row, "detail"),
        text(row, "fix_specific"),
        text(row, "fix"),
        text(row, "kind"),
        text(row, "site_count"),
        locations_text(row),
        text(row, "severity_adjustments"),
        text(row, "carried_from"),
    ];
    if false_positive {
        cells.push(text(row, "fp_reason"));
    }
    cells
}

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
        let rendered: Vec<&str> = FINDING_RENDERED.to_vec();
        let json_only: Vec<&str> = FINDING_EXTRA_KEYS.to_vec();
        // `kind_payload_placeholder` and `disposition_kind` are tolerated only if absent; they
        // are filtered out of the stale check below by being in json_only, not rendered.
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

    let header_refs: Vec<&str> = FINDING_HEADERS.to_vec();

    let grid = read_sheet(xlsx, "All Findings");
    let sheet = table(&grid, &header_refs).expect("All Findings carries its header row");
    let expected: Vec<Vec<String>> = on_findings_sheet
        .iter()
        .map(|r| finding_cells(r, false))
        .collect();
    assert_eq!(
        sheet.len(),
        expected.len(),
        "All Findings: worksheet and JSON disagree on the row count"
    );
    assert_eq!(
        sheet, expected,
        "All Findings: worksheet cells differ from JSON"
    );

    let mut fp_headers = header_refs.clone();
    fp_headers.push("FP Reason");
    let grid = read_sheet(xlsx, "False Positives");
    let sheet = table(&grid, &fp_headers).expect("False Positives carries its header row");
    let expected: Vec<Vec<String>> = false_positives
        .iter()
        .map(|r| finding_cells(r, true))
        .collect();
    assert_eq!(sheet.len(), expected.len(), "False Positives row count");
    assert_eq!(
        sheet, expected,
        "False Positives: worksheet cells differ from JSON"
    );
}

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
        ],
        &[],
    );
    let headers = [
        "Area",
        "What was checked",
        "Convention your codebase follows",
        "Sites deviating",
        "Sites assessed",
        "Highest severity",
        "Where",
        "Recommended fix",
        "Rule ID",
    ];
    let expected: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                text(r, "category"),
                text(r, "rule_title"),
                text(r, "convention_label"),
                count_text(r, "deviating_sites"),
                if r["assessed_sites"].is_null() {
                    "not counted".to_string()
                } else {
                    count_text(r, "assessed_sites")
                },
                text(r, "worst_severity"),
                r["locations"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|l| l.as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join("\n"),
                text(r, "recommended_fix"),
                text(r, "rule_id"),
            ]
        })
        .collect();
    let sheet = table(&read_sheet(xlsx, "Deviations"), &headers).expect("Deviations header row");
    assert_eq!(sheet.len(), expected.len(), "Deviations row count");
    assert_eq!(
        sheet, expected,
        "Deviations: worksheet cells differ from JSON"
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
    let headers = [
        "Area",
        "What was checked",
        "Status",
        "Convention your codebase follows",
        "Patterns in use",
        "What we observed",
        "Detail",
        "Sites",
        "Also found at",
        "Rule ID",
    ];
    let expected: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                text(r, "category"),
                text(r, "rule_title"),
                text(r, "status"),
                text(r, "convention_label"),
                text(r, "patterns_in_use"),
                text(r, "headline"),
                text(r, "detail"),
                count_text(r, "sites"),
                text(r, "also_found_at"),
                text(r, "rule_id"),
            ]
        })
        .collect();
    let sheet = table(&read_sheet(xlsx, "Conventions"), &headers).expect("Conventions header row");
    assert_eq!(sheet.len(), expected.len(), "Conventions row count");
    assert_eq!(
        sheet, expected,
        "Conventions: worksheet cells differ from JSON"
    );
}

fn check_dependencies(xlsx: &[u8], rows: &[Value]) {
    assert_uniform_keys("dependencies", rows);
    // `ownership` is the machine tag behind the worksheet's "Recommended action" wording;
    // the sheet renders the consequence (the action), not the tag.
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
        ],
        &["ownership"],
    );
    let headers = [
        "Package",
        "Advisory",
        "Severity",
        "Repo",
        "Fixed in",
        "Recommended action",
    ];
    let mut expected: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let fixed = r["fixed_version"].as_str();
            let action = match (text(r, "action").as_str(), fixed) {
                ("", Some(v)) => format!("Upgrade to >= {v}."),
                (a, _) => a.to_string(),
            };
            vec![
                text(r, "package"),
                text(r, "advisory"),
                title_case(&text(r, "severity")),
                text(r, "repo"),
                fixed.unwrap_or("(no patch yet)").to_string(),
                action,
            ]
        })
        .collect();
    let mut sheet =
        table(&read_sheet(xlsx, "Dependencies"), &headers).expect("Dependencies header row");
    assert_eq!(sheet.len(), expected.len(), "Dependencies row count");
    // The worksheet re-sorts by severity then package; the JSON keeps partition order. The
    // set of rows is what must agree.
    expected.sort();
    sheet.sort();
    assert_eq!(
        sheet, expected,
        "Dependencies: worksheet cells differ from JSON"
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
    let headers = [
        "What was checked",
        "Why",
        "What we observed",
        "Detail",
        "Rule ID",
    ];
    let expected: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                text(r, "rule_title"),
                text(r, "why"),
                text(r, "headline"),
                text(r, "detail"),
                text(r, "rule_id"),
            ]
        })
        .collect();
    let sheet = table(&read_sheet(xlsx, "Coverage"), &headers).unwrap_or_default();
    assert_eq!(sheet.len(), expected.len(), "not-assessed row count");
    assert_eq!(
        sheet, expected,
        "not-assessed: worksheet cells differ from JSON"
    );
}

fn check_coverage(xlsx: &[u8], rows: &[Value]) {
    assert_uniform_keys("coverage", rows);
    // `citation_kind` is the machine tag behind the worksheet's "How we checked it" wording.
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
    let headers = [
        "Area",
        "What was checked",
        "Result",
        "How we checked it",
        "Standard cited",
        "Rule ID",
    ];
    let expected: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            let n = r["findings_count"].as_u64().unwrap();
            vec![
                text(r, "category"),
                text(r, "title"),
                match n {
                    0 => "Clean \u{2014} nothing found".to_string(),
                    1 => "1 site found".to_string(),
                    n => format!("{n} sites found"),
                },
                match r["citation_kind"].as_str().unwrap() {
                    "grounded" => "Deterministic",
                    "preview" => "Preview",
                    _ => "AI-advisory",
                }
                .to_string(),
                text(r, "citation_label"),
                text(r, "rule_id"),
            ]
        })
        .collect();
    let sheet = table(&read_sheet(xlsx, "Coverage"), &headers).unwrap_or_default();
    assert_eq!(sheet.len(), expected.len(), "coverage row count");
    assert_eq!(
        sheet, expected,
        "coverage: worksheet cells differ from JSON"
    );
}

fn check_rules_applied(xlsx: &[u8], rows: &[Value]) {
    assert_uniform_keys("rules-applied", rows);
    assert_key_accounting(
        "rules-applied",
        rows,
        &["rule_id", "chosen", "evidence", "applicable"],
        &[],
    );
    let headers = ["Rule ID", "Chosen alternative", "Evidence", "Applicable"];
    let expected: Vec<Vec<String>> = rows
        .iter()
        .map(|r| {
            vec![
                text(r, "rule_id"),
                text(r, "chosen"),
                text(r, "evidence"),
                if r["applicable"].as_bool().unwrap() {
                    "Yes"
                } else {
                    "No"
                }
                .to_string(),
            ]
        })
        .collect();
    let sheet = table(&read_sheet(xlsx, "Coverage"), &headers).unwrap_or_default();
    assert_eq!(sheet.len(), expected.len(), "rules-applied row count");
    assert_eq!(
        sheet, expected,
        "rules-applied: worksheet cells differ from JSON"
    );
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
        xlsx: build_workbook(report, dispositions, corpus, &opts).unwrap(),
        json: by_dataset(
            build_json_exports(report, dispositions, corpus, &opts).unwrap(),
            "json",
            "json",
        ),
        csv: by_dataset(
            build_csv_exports(report, dispositions, corpus, &opts).unwrap(),
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

    // The dataset set is exactly the eight, in BOTH text formats.
    let expected: BTreeSet<String> = DATASETS.iter().map(|s| s.to_string()).collect();
    assert_eq!(a.json.keys().cloned().collect::<BTreeSet<_>>(), expected);
    assert_eq!(a.csv.keys().cloned().collect::<BTreeSet<_>>(), expected);

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
    check_coverage(&a.xlsx, &rows["coverage"]);
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
    for (sheet, headers) in [
        ("All Findings", FINDING_HEADERS.to_vec()),
        (
            "Dependencies",
            vec![
                "Package",
                "Advisory",
                "Severity",
                "Repo",
                "Fixed in",
                "Recommended action",
            ],
        ),
    ] {
        let rows = table(&read_sheet(&a.xlsx, sheet), &headers)
            .unwrap_or_else(|| panic!("{sheet} must keep its header row when empty"));
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
