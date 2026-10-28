//! The release gate's own tests (docs/design/2026-10-23_secret-redaction-and-release-gate.md
//! section 8.5). A real package is built through `assemble_package` (real workbook, datasets,
//! provenance and README) around a stand-in PDF, so these run without Typst; the gate checks the
//! PDF's DATA (`data_json`), which is where the PDF's content lives. Every secret is built at
//! runtime by `camerata_secrets::fixtures`.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;

use camerata_secrets::fixtures::{planted, planted_all, Planted};
use camerata_secrets::SecretShape;
use serde_json::{json, Value};

use super::*;
use crate::onboard::{AuditedRef, Finding, LanguageVolume, ScanProvenance, ScanReport};
use crate::report_export::{build_report_json, render_data_json};

const STEM: &str = "acme-inspection";
const STAND_IN_PDF: &[u8] = b"%PDF-1.7 stand-in for the gate tests";

fn sealed_report() -> SealedReport {
    scan_report().seal()
}

pub(super) fn scan_report() -> ScanReport {
    let findings = vec![Finding {
        repo: "acme/portal".to_string(),
        path: "src/db.rs".to_string(),
        line: 12,
        rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        severity: "high".to_string(),
        snippet: "format!(\"select {}\", x)".to_string(),
        detail: "raw SQL concatenation".to_string(),
        ..Finding::default()
    }];
    ScanReport {
        repos: vec!["acme/portal".to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 0,
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
            audit_model: None,
            calibration_model: None,
            clone_judge_model: None,
            mode: "parallel".to_string(),
            thorough: false,
            deep: false,
            rules_fingerprint: "fp".to_string(),
            audited_rule_ids: vec!["SEC-NO-RAW-SQL-CONCAT-1".to_string()],
            camerata_version: "0.0.0-test".to_string(),
            osv_scanner_version: None,
            vulnerability_data: None,
            started_at: "2026-10-21T00:00:00Z".to_string(),
            finished_at: "2026-10-21T00:05:00Z".to_string(),
            model_sessions: Vec::new(),
        },
        recommendations: HashMap::new(),
        failed_passes: Vec::new(),
        ledger: crate::scan_ledger::ScanLedger::new(),
    }
}

/// Report options with the client name every export needs (synthetic).
pub(super) fn client_options() -> ReportOptions {
    ReportOptions {
        client_name: "Example Client Ltd".to_string(),
        ..ReportOptions::default()
    }
}

/// A clean package, built by the production assembly around a stand-in PDF.
pub(super) fn package() -> ProductPackage {
    let sealed = sealed_report();
    let options = client_options();
    let json = build_report_json(&sealed, &HashMap::new(), None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &sealed,
        &HashMap::new(),
        None,
        &options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles")
}

fn released() -> ReleasedZip {
    release(package()).expect("a clean package must clear the gate")
}

/// 22a: the backend and provider in `ReportOptions` reach the package's provenance, and the
/// released package (gate included) states them in the manifest and the README alike.
#[test]
fn the_backend_and_provider_in_the_options_reach_the_manifest_and_readme() {
    let sealed = sealed_report();
    let options = ReportOptions {
        model: Some(crate::ModelRecord::anthropic(
            crate::llm::ProjectBackend::Api,
        )),
        ..client_options()
    };
    let json = build_report_json(&sealed, &HashMap::new(), None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        &sealed,
        &HashMap::new(),
        None,
        &options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles");
    assert!(package.readme.contains("Model backend: api\n"));
    assert!(package.readme.contains("Model provider: anthropic\n"));
    let manifest = package.provenance.inspection_json();
    assert_eq!(manifest["model_backend"], "api");
    assert_eq!(manifest["model_provider"], "anthropic");
    release(package).expect("the gate's agreement check passes with the model lines");
}

fn blocked(package: ProductPackage) -> GateFailure {
    match release(package) {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("expected the gate to block, got {other}"),
        Ok(_) => panic!("expected the gate to block, but the package was released"),
    }
}

// ---- planting a secret on each client-visible surface --------------------------------------

#[derive(Debug, Clone, Copy)]
pub(super) enum Surface {
    DataJson,
    FindingsJson,
    DatasetJson,
    DatasetCsv,
    XlsxSharedString,
    Readme,
    Stem,
}

pub(super) const ALL_SURFACES: [Surface; 7] = [
    Surface::DataJson,
    Surface::FindingsJson,
    Surface::DatasetJson,
    Surface::DatasetCsv,
    Surface::XlsxSharedString,
    Surface::Readme,
    Surface::Stem,
];

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Rewrite one part of an xlsx (a zip), keeping every other part byte for byte.
fn rewrite_xlsx_part(xlsx: &[u8], part: &str, edit: impl FnOnce(&str) -> String) -> Vec<u8> {
    let entries = read_zip_entries(xlsx).expect("the workbook is a zip");
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut edit = Some(edit);
    for (name, bytes) in entries {
        writer.start_file(name.clone(), options).expect("start");
        if name == part {
            let text = String::from_utf8(bytes).expect("utf8 part");
            let edited = edit.take().expect("one edit")(&text);
            writer.write_all(edited.as_bytes()).expect("write");
        } else {
            writer.write_all(&bytes).expect("write");
        }
    }
    writer.finish().expect("finish").into_inner()
}

/// Replace dataset `name` (both its CSV and its JSON) with one row whose only field is `text`.
fn set_dataset(package: &mut ProductPackage, name: &str, text: &str) {
    let json = serde_json::to_vec(&json!([{ "snippet": text }])).expect("serializes");
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record(["snippet"]).expect("header");
    writer.write_record([text]).expect("row");
    let csv = writer.into_inner().expect("csv bytes");
    for (path, bytes) in package.datasets.iter_mut() {
        if *path == format!("json/{name}.json") {
            *bytes = json.clone();
        } else if *path == format!("csv/{name}.csv") {
            *bytes = csv.clone();
        }
    }
}

/// Plant `text` on `surface` of an otherwise clean package.
pub(super) fn plant(package: &mut ProductPackage, surface: Surface, text: &str) {
    match surface {
        Surface::DataJson => {
            // A BRAND NEW top-level field nobody ever listed: the walk must still find it.
            let mut data: Value = serde_json::from_str(&package.data_json).expect("data parses");
            data["a_field_added_tomorrow"] = json!({ "note": text });
            package.data_json = data.to_string();
        }
        Surface::FindingsJson => {
            let mut doc: Value = serde_json::from_slice(&package.findings_json).expect("parses");
            doc["a_field_added_tomorrow"] = json!(text);
            package.findings_json = serde_json::to_vec_pretty(&doc).expect("serializes");
        }
        // A dataset is planted in BOTH formats so the CSV and JSON stay in parity: the only
        // problem a test should see is the secret.
        Surface::DatasetJson => set_dataset(package, "deviations", text),
        Surface::DatasetCsv => set_dataset(package, "conventions", text),
        Surface::XlsxSharedString => {
            let escaped = xml_escape(text);
            package.xlsx = rewrite_xlsx_part(&package.xlsx, "xl/sharedStrings.xml", |xml| {
                xml.replacen("</sst>", &format!("<si><t>{escaped}</t></si></sst>"), 1)
            });
        }
        Surface::Readme => package.readme.push_str(&format!("\nNote: {text}\n")),
        Surface::Stem => package.stem = format!("camerata-{}", text.replace('\n', "-")),
    }
}

/// The redacted form the gate should show for `text` (one line).
fn expected_evidence(text: &str) -> String {
    let redacted = camerata_secrets::redact(text);
    assert!(
        !redacted.redactions.is_empty(),
        "the fixture must be a secret"
    );
    redacted.redactions[0]
        .form
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn secret_problems(failure: &GateFailure) -> Vec<&Problem> {
    failure
        .report
        .checks
        .iter()
        .filter(|o| o.check == CheckId::Secrets)
        .flat_map(|o| o.problems.iter())
        .collect()
}

/// Whether any line of the planted value appears in `haystack`.
fn leaked(haystack: &str, p: &Planted) -> bool {
    p.value.lines().any(|line| haystack.contains(line))
}

// ---- the happy path -----------------------------------------------------------------------------

#[test]
fn release_passes_a_clean_package_and_reports_eight_checks() {
    let z = released();
    assert!(z.report.is_clean());
    assert_eq!(
        z.report.check_names(),
        vec![
            "secrets",
            "dataset set",
            "dataset parity",
            "row parity",
            "severity and kind counts",
            "confidentiality marking",
            "manifest",
            "provenance"
        ]
    );
    assert_eq!(&z.bytes[0..2], b"PK");
}

#[test]
fn the_readme_and_manifest_state_the_release_checks_that_ran() {
    let z = released();
    let entries = read_zip_entries(&z.bytes).expect("zip");
    let readme = String::from_utf8_lossy(&entries["README.txt"]).to_string();
    assert!(
        readme.contains(
            "Release checks: secrets, dataset set, dataset parity, row parity, severity and \
             kind counts, confidentiality marking, manifest, provenance (all passed)"
        ),
        "{readme}"
    );
    let manifest: Value = serde_json::from_slice(&entries["manifest.json"]).expect("manifest");
    assert_eq!(
        manifest["inspection"]["release_checks"],
        json!(ManifestProvenance::release_checks())
    );
}

// ---- secrets: every surface ---------------------------------------------------------------------

#[test]
fn release_blocks_a_secret_planted_in_every_surface_with_an_exact_problem() {
    let p = planted(SecretShape::SupabaseSecretKey);
    let evidence = expected_evidence(&p.value);
    for surface in ALL_SURFACES {
        let mut pkg = package();
        plant(&mut pkg, surface, &p.text);
        let failure = blocked(pkg);
        let problems = secret_problems(&failure);
        assert!(!problems.is_empty(), "{surface:?}: no secrets problem");
        let (file, location) = match surface {
            Surface::DataJson => ("PDF data (data.json)", "a_field_added_tomorrow.note"),
            Surface::FindingsJson => ("findings.json", "a_field_added_tomorrow"),
            Surface::DatasetJson => ("json/deviations.json", "$[0].snippet"),
            Surface::DatasetCsv => ("csv/conventions.csv", "row 1, column \"snippet\""),
            Surface::XlsxSharedString => ("acme-inspection-findings.xlsx", "shared string #"),
            Surface::Readme => ("README.txt", "line "),
            Surface::Stem => ("(archive member name)", ""),
        };
        let hit = problems
            .iter()
            .find(|q| q.file == file && q.location.starts_with(location))
            .unwrap_or_else(|| {
                panic!("{surface:?}: no problem at {file} {location}: {problems:?}")
            });
        if matches!(surface, Surface::Stem) {
            // The stem is spliced into member names (`{stem}-findings.xlsx`), so the key is
            // followed by name characters the credential-named-line shape also reads: the shape
            // and the form can differ, but the value must never be shown.
            let shown = format!("{hit:?}");
            assert!(!leaked(&shown, &p), "{shown}");
            continue;
        }
        assert_eq!(
            hit.evidence.as_deref(),
            Some(evidence.as_str()),
            "{surface:?}"
        );
        assert_eq!(hit.message, "Supabase secret key present in a client file");
    }
}

#[test]
fn secrets_check_walks_also_locations_not_a_named_field_list() {
    // The hearth regression: the old sanitizer named path, snippet, detail and captures, so a
    // secret in `also_locations[*].snippet` or `fix_specific` shipped. The gate walks every
    // string, so a field added tomorrow is covered too.
    let p = planted(SecretShape::StripeLiveSecretKey);
    let mut pkg = package();
    let mut doc: Value = serde_json::from_slice(&pkg.findings_json).expect("parses");
    doc["findings"] = json!([{
        "also_locations": [{ "snippet": "clean" }, { "snippet": p.text }],
        "fix_specific": format!("Replace {} now", p.value),
        "a_field_added_tomorrow": { "deep": [p.value] },
    }]);
    pkg.findings_json = serde_json::to_vec_pretty(&doc).expect("serializes");
    let failure = blocked(pkg);
    let locations: Vec<&str> = secret_problems(&failure)
        .iter()
        .filter(|q| q.file == "findings.json")
        .map(|q| q.location.as_str())
        .collect();
    for expected in [
        "findings[0].also_locations[1].snippet",
        "findings[0].fix_specific",
        "findings[0].a_field_added_tomorrow.deep[0]",
    ] {
        assert!(
            locations.contains(&expected),
            "missing {expected} in {locations:?}"
        );
    }
}

#[test]
fn secrets_check_locates_an_xlsx_hit_by_sheet_and_cell() {
    let p = planted(SecretShape::GitHubToken);
    let mut pkg = package();
    let cells = xlsx_cells(&pkg.xlsx).expect("cells");
    let target = cells
        .iter()
        .find(|c| c.sheet == "All Findings" && c.text == "raw SQL concatenation")
        .or_else(|| cells.iter().find(|c| c.sheet == "All Findings"))
        .expect("an All Findings cell")
        .clone();
    let needle = format!(">{}<", xml_escape(&target.text));
    let replacement = format!(">{}<", xml_escape(&p.value));
    pkg.xlsx = rewrite_xlsx_part(&pkg.xlsx, "xl/sharedStrings.xml", |xml| {
        assert!(xml.contains(&needle), "the target string is in the pool");
        xml.replacen(&needle, &replacement, 1)
    });
    let failure = blocked(pkg);
    let located = secret_problems(&failure)
        .iter()
        .any(|q| q.location == format!("sheet {:?} cell {}", target.sheet, target.reference));
    assert!(located, "no problem located at the cell: {failure}");
}

#[test]
fn secrets_check_ignores_twins_in_every_surface() {
    let twin = "SUPABASE_KEY=sb_secret_your_key_here_please";
    for surface in ALL_SURFACES {
        let mut pkg = package();
        plant(&mut pkg, surface, twin);
        let result = release(pkg);
        assert!(
            result.is_ok(),
            "{surface:?}: a placeholder must not block the export: {}",
            result.err().map(|e| e.to_string()).unwrap_or_default()
        );
    }
}

#[test]
fn a_zero_width_character_inside_a_key_does_not_hide_it_from_the_gate() {
    let p = planted(SecretShape::SupabaseSecretKey);
    let (head, tail) = p.value.split_at(14);
    let hidden = format!("{head}\u{200B}{tail}");
    for surface in [Surface::DataJson, Surface::Readme, Surface::DatasetCsv] {
        let mut pkg = package();
        plant(&mut pkg, surface, &hidden);
        let failure = blocked(pkg);
        assert!(!secret_problems(&failure).is_empty(), "{surface:?}");
    }
}

#[test]
fn release_reports_every_problem_not_just_the_first() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let mut pkg = package();
    for surface in [Surface::DataJson, Surface::FindingsJson, Surface::Readme] {
        plant(&mut pkg, surface, &p.text);
    }
    let failure = blocked(pkg);
    let files: std::collections::BTreeSet<&str> = secret_problems(&failure)
        .iter()
        .map(|q| q.file.as_str())
        .collect();
    for expected in ["PDF data (data.json)", "findings.json", "README.txt"] {
        assert!(
            files.contains(expected),
            "{expected} missing from {files:?}"
        );
    }
    assert!(failure.report.problem_count() >= 3);
}

#[test]
fn gate_failure_message_never_contains_the_secret() {
    for p in planted_all() {
        for surface in ALL_SURFACES {
            let mut pkg = package();
            plant(&mut pkg, surface, &p.text);
            let failure = blocked(pkg);
            let display = failure.to_string();
            assert!(
                !leaked(&display, &p),
                "{:?} on {surface:?} leaked into the Display block:\n{display}",
                p.shape
            );
            let body = failure.report.problems_json().to_string();
            assert!(
                !leaked(&body, &p),
                "{:?} on {surface:?} leaked into the HTTP problems body",
                p.shape
            );
            assert!(display.starts_with("EXPORT BLOCKED by the release gate for "));
            assert!(display.contains("[secrets"));
        }
    }
}

#[test]
fn a_problem_redacts_even_a_value_handed_to_it_by_mistake() {
    let p = planted(SecretShape::GitHubToken);
    let q = Problem::new(&p.text, &p.text, &p.text, Some(p.text.clone()));
    let json = serde_json::to_string(&q).expect("serializes");
    assert!(!json.contains(&p.value), "{json}");
}

#[test]
fn the_failure_block_has_the_documented_shape() {
    let p = planted(SecretShape::SupabaseSecretKey);
    let mut pkg = package();
    plant(&mut pkg, Surface::FindingsJson, &p.text);
    let display = blocked(pkg).to_string();
    let first = display.lines().next().unwrap_or_default();
    assert_eq!(
        first,
        "EXPORT BLOCKED by the release gate for acme-inspection (1 problem)"
    );
    assert!(
        display.contains("findings.json \u{203a} a_field_added_tomorrow"),
        "{display}"
    );
    assert!(
        display.contains("Supabase secret key present in a client file: sb_secret_\u{2026}"),
        "{display}"
    );
    assert!(display.contains("Nothing was written."), "{display}");
}

// ---- the consistency checks -----------------------------------------------------------------------

fn entries_of(z: &ReleasedZip) -> BTreeMap<String, Vec<u8>> {
    read_zip_entries(&z.bytes).expect("zip")
}

#[test]
fn dataset_set_check_flags_a_missing_and_an_extra_dataset() {
    let mut entries = entries_of(&released());
    assert!(dataset_set_problems(&entries).is_empty(), "control");
    entries.remove("csv/deviations.csv");
    entries.insert("json/surprise.json".to_string(), b"[]".to_vec());
    let problems = dataset_set_problems(&entries);
    assert!(problems
        .iter()
        .any(|q| q.file == "csv/deviations.csv" && q.message.contains("missing")));
    assert!(problems
        .iter()
        .any(|q| q.file == "json/surprise.json" && q.message.contains("unexpected")));
}

#[test]
fn dataset_set_check_flags_a_pdf_without_the_magic_and_an_empty_findings_json() {
    let mut entries = entries_of(&released());
    entries.insert(format!("{STEM}.pdf"), b"not a pdf".to_vec());
    entries.insert("findings.json".to_string(), Vec::new());
    let problems = dataset_set_problems(&entries);
    assert!(problems.iter().any(|q| q.message.contains("%PDF-")));
    assert!(problems
        .iter()
        .any(|q| q.file == "findings.json" && q.message.contains("empty")));
}

#[test]
fn dataset_parity_check_flags_a_cell_disagreement() {
    let mut entries = entries_of(&released());
    assert!(dataset_parity_problems(&entries).is_empty(), "control");
    let csv = String::from_utf8(entries["csv/all-findings.csv"].clone()).expect("utf8");
    entries.insert(
        "csv/all-findings.csv".to_string(),
        csv.replace("SEC-NO-RAW-SQL-CONCAT-1", "SEC-TAMPERED-1")
            .into_bytes(),
    );
    let problems = dataset_parity_problems(&entries);
    assert!(
        problems.iter().any(|q| q.file == "csv/all-findings.csv"
            && q.location.starts_with("row 1")
            && q.message.contains("differs")),
        "{problems:?}"
    );
}

#[test]
fn dataset_parity_check_flags_findings_json_disagreeing_with_all_findings() {
    let mut entries = entries_of(&released());
    let mut doc: Value = serde_json::from_slice(&entries["findings.json"]).expect("parses");
    doc["findings"][0]["severity"] = json!("tampered");
    entries.insert(
        "findings.json".to_string(),
        serde_json::to_vec_pretty(&doc).expect("serializes"),
    );
    let problems = dataset_parity_problems(&entries);
    assert!(
        problems
            .iter()
            .any(|q| q.file == "findings.json" && q.message.contains("all-findings")),
        "{problems:?}"
    );
}

#[test]
fn row_parity_check_flags_a_pdf_row_absent_from_findings_json() {
    let pkg = package();
    assert!(
        row_parity(&pkg.report_json, &pkg.findings_export).is_empty(),
        "control"
    );
    let mut json = pkg.report_json.clone();
    for bucket in [
        &mut json.matrix.do_now,
        &mut json.matrix.do_next,
        &mut json.matrix.plan,
        &mut json.matrix.accepted,
        &mut json.matrix.informational,
        &mut json.matrix.held,
    ] {
        bucket.clear();
    }
    let problems = row_parity(&json, &pkg.findings_export);
    assert!(
        problems
            .iter()
            .any(|q| q.file == "PDF data" && q.message.contains("missing from the PDF")),
        "{problems:?}"
    );
}

fn rezip(entries: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        writer.start_file(name.clone(), options).expect("start");
        writer.write_all(bytes).expect("write");
    }
    writer.finish().expect("finish").into_inner()
}

#[test]
fn manifest_check_flags_one_corrupted_byte_in_any_member() {
    let entries = entries_of(&released());
    assert!(verify_manifest(&entries).is_empty(), "control");
    for target in entries.keys().filter(|n| *n != "manifest.json") {
        let mut tampered = entries.clone();
        let bytes = tampered.get_mut(target).expect("member");
        let at = bytes.len() / 2;
        bytes[at] ^= 0x01;
        let problems = verify_manifest(&tampered);
        assert_eq!(
            problems.iter().map(|q| q.file.as_str()).collect::<Vec<_>>(),
            vec![target.as_str()],
            "flipping one bit of {target} must be flagged, and only {target}"
        );
    }
}

#[test]
fn provenance_check_flags_a_readme_manifest_mismatch() {
    let entries = entries_of(&released());
    let manifest: Value = serde_json::from_slice(&entries["manifest.json"]).expect("manifest");
    let readme = String::from_utf8(entries["README.txt"].clone()).expect("utf8");
    assert!(
        verify_readme_provenance(&readme, &manifest).is_empty(),
        "control"
    );

    let wrong_engine = readme.replace("Engine version: camerata ", "Engine version: camerata 9");
    let problems = verify_readme_provenance(&wrong_engine, &manifest);
    assert!(
        problems.iter().any(|q| q.location == "Engine version"),
        "{problems:?}"
    );

    let dropped_checks: String = readme
        .lines()
        .filter(|l| !l.starts_with("Release checks:"))
        .collect::<Vec<_>>()
        .join("\n");
    let problems = verify_readme_provenance(&dropped_checks, &manifest);
    assert!(
        problems.iter().any(|q| q.location == "Release checks"),
        "{problems:?}"
    );
}

#[test]
fn audit_zip_over_a_released_zip_is_clean() {
    let z = released();
    let report = audit_zip(&z.bytes, None);
    assert!(
        report.is_clean(),
        "{:?}",
        report.problems().collect::<Vec<_>>()
    );
    assert_eq!(
        report.checks.len(),
        7,
        "row parity needs the in-memory rows"
    );
}

#[test]
fn audit_zip_flags_a_tampered_member_and_a_planted_secret_in_the_pdf_text() {
    let z = released();
    let mut entries = entries_of(&z);
    entries.insert(
        "README.txt".to_string(),
        b"edited after the manifest".to_vec(),
    );
    let report = audit_zip(&rezip(&entries), None);
    assert!(!report.is_clean());
    assert!(report
        .problems()
        .any(|(check, q)| check == CheckId::Manifest && q.file == "README.txt"));

    // The CLI extracts the PDF's text; a key in it is found, including one Typst wrapped.
    let p = planted(SecretShape::StripeLiveSecretKey);
    let (a, b) = p.value.split_at(30);
    let wrapped = format!("The key is {a}\n{b} in the file.");
    let report = audit_zip(&z.bytes, Some(&wrapped));
    assert!(
        report
            .problems()
            .any(|(check, q)| check == CheckId::Secrets && q.file.ends_with(".pdf")),
        "a key wrapped across two lines of PDF text must still be found"
    );
}

#[test]
fn audit_zip_reports_an_unreadable_archive_without_panicking() {
    let report = audit_zip(b"this is not a zip", None);
    assert!(!report.is_clean());
}

#[test]
fn release_never_returns_bytes_on_failure() {
    let p = planted(SecretShape::GitHubToken);
    let mut pkg = package();
    plant(&mut pkg, Surface::Readme, &p.text);
    // The only failure value is a GateFailure: it holds the stem and the report, never a byte
    // buffer, so there is nothing to write by mistake.
    let failure = blocked(pkg);
    assert_eq!(failure.stem, STEM);
    assert!(!failure.report.is_clean());
}

#[test]
fn check_pdf_data_blocks_a_secret_in_the_pdf_only_route() {
    let p = planted(SecretShape::SupabaseSecretKey);
    let clean = package().data_json;
    assert!(check_pdf_data(STEM, &clean).is_ok());
    let mut data: Value = serde_json::from_str(&clean).expect("parses");
    data["curated_findings"] = json!([{ "sites": [{ "snippet": p.text }] }]);
    let failure = check_pdf_data(STEM, &data.to_string()).expect_err("must block");
    assert!(!leaked(&failure.to_string(), &p));
    assert!(failure
        .to_string()
        .contains("curated_findings[0].sites[0].snippet"));
}

// ---- source pins: one path to zip bytes, one path to a PDF ------------------------------------

/// The production part of a source file: everything before its inline `#[cfg(test)]` module.
fn production_part(source: &str) -> &str {
    source
        .find("\n#[cfg(test)]\nmod ")
        .map_or(source, |at| &source[..at])
}

fn rust_sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Every production call site of `needle(` across `crates/*/src`, as `file:line`. Definitions
/// (`fn needle(`) and comment lines are not call sites.
fn production_call_sites(needle: &str) -> Vec<String> {
    let crates = std::fs::canonicalize(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(".."))
        .expect("the crates directory");
    let mut files = Vec::new();
    for krate in std::fs::read_dir(&crates).expect("crates dir").flatten() {
        rust_sources(&krate.path().join("src"), &mut files);
    }
    assert!(files.len() > 50, "the scan must see the workspace sources");
    let call = format!("{needle}(");
    let mut sites = Vec::new();
    for file in files {
        // Test-only source files and the gate's own tests are not production code.
        let name = file.to_string_lossy().to_string();
        if name.contains("/tests/") || name.ends_with("tests.rs") || name.contains("_tests") {
            continue;
        }
        let source = std::fs::read_to_string(&file).unwrap_or_default();
        for (i, line) in production_part(&source).lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with("///") {
                continue;
            }
            if line.contains(&call) && !line.contains(&format!("fn {call}")) {
                let short = name.rsplit("/crates/").next().unwrap_or(&name);
                sites.push(format!("{short}:{}", i + 1));
            }
        }
    }
    sites
}

#[test]
fn build_product_zip_is_called_from_release_only() {
    let sites = production_call_sites("build_product_zip");
    let outside: Vec<&String> = sites
        .iter()
        .filter(|s| !s.starts_with("server/src/release_gate.rs"))
        // The doc(hidden) test wrapper forwards to it; nothing in production calls the wrapper
        // (pinned by the next test).
        .filter(|s| !s.starts_with("server/src/lib.rs"))
        .collect();
    assert!(
        outside.is_empty(),
        "build_product_zip( called outside the gate: {outside:?}"
    );
    let in_gate = sites
        .iter()
        .filter(|s| s.starts_with("server/src/release_gate.rs"))
        .count();
    assert_eq!(in_gate, 1, "release() is the one caller: {sites:?}");
}

#[test]
fn the_unreleased_zip_wrapper_is_never_called_by_production_code() {
    let sites = production_call_sites("assemble_unreleased_zip");
    assert!(
        sites.is_empty(),
        "production code builds an ungated zip: {sites:?}"
    );
}

#[test]
fn compile_pdf_from_data_is_called_from_release_and_the_pdf_route_only() {
    let sites = production_call_sites("compile_pdf_from_data");
    let allowed = |s: &String| {
        s.starts_with("server/src/release_gate.rs")
            || s.starts_with("server/src/lib.rs")
            || s.starts_with("server/src/report_export.rs")
    };
    let outside: Vec<&String> = sites.iter().filter(|s| !allowed(s)).collect();
    assert!(
        outside.is_empty(),
        "compile_pdf_from_data( called elsewhere: {outside:?}"
    );
    // lib.rs: exactly the PDF-only route, and it checks data.json first.
    let lib = production_part(include_str!("../lib.rs"));
    let route_start = lib
        .find("async fn export_audit_report(")
        .expect("the PDF route");
    let route = &lib[route_start..];
    let gate_at = route
        .find("check_pdf_data(")
        .expect("the PDF route is gated");
    let compile_at = route
        .find("compile_pdf_from_data(")
        .expect("the PDF route compiles");
    assert!(
        gate_at < compile_at,
        "the gate must run BEFORE the PDF is compiled"
    );
}

#[test]
fn the_wrapper_compile_pdf_is_not_called_by_production_code_outside_report_export() {
    let sites = production_call_sites("compile_pdf");
    let outside: Vec<&String> = sites
        .iter()
        .filter(|s| !s.starts_with("server/src/report_export.rs"))
        .collect();
    assert!(
        outside.is_empty(),
        "compile_pdf( bypasses the gate: {outside:?}"
    );
}

// ---- the HTTP routes ----------------------------------------------------------------------------

/// A scan whose finding PATH carries a secret-shaped name. Paths are client text no producer
/// redacts, so this is a real way for a value to reach every artifact, and exactly what the gate
/// exists to stop.
fn app_with_a_leaking_path(p: &Planted) -> (crate::AppState, String) {
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
    let mut report = scan_report();
    report.findings[0].path = format!("src/{}.rs", p.value);
    state.set_last_scan(project.id.clone(), report);
    (state, project.id)
}

fn request() -> axum::Json<crate::AuditReportReq> {
    axum::Json(crate::AuditReportReq {
        dispositions: HashMap::new(),
        options: ReportOptions::default(),
    })
}

fn typst_on_path() -> bool {
    camerata_test_env::typst_on_path()
}

async fn body_json(resp: axum::response::Response) -> Value {
    use http_body_util::BodyExt;
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    serde_json::from_slice(&bytes).expect("a JSON body")
}

#[tokio::test]
async fn export_audit_report_pdf_route_is_gated_on_data_json() {
    // No Typst needed: the gate runs on data.json BEFORE the PDF is compiled.
    let p = planted(SecretShape::SupabaseSecretKey);
    let (state, id) = app_with_a_leaking_path(&p);
    let resp = crate::export_audit_report(
        axum::extract::State(state),
        axum::extract::Path(id),
        request(),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    let body = body_json(resp).await;
    assert_eq!(body["gate"], "release");
    assert_eq!(body["ok"], false);
    assert!(
        !body.to_string().contains(&p.value),
        "the 422 body must never carry the value: {body}"
    );
    assert!(body["message"]
        .as_str()
        .is_some_and(|m| m.starts_with("EXPORT BLOCKED by the release gate")));
}

#[tokio::test]
async fn export_product_returns_422_with_the_problem_list_when_blocked() {
    if !typst_on_path() {
        eprintln!("export_product 422 test: skipping, typst is not on PATH");
        return;
    }
    let p = planted(SecretShape::SupabaseSecretKey);
    let (state, id) = app_with_a_leaking_path(&p);
    let resp = crate::export_product(
        axum::extract::State(state),
        axum::extract::Path(id),
        request(),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .map(|v| v.as_bytes()),
        Some(&b"application/json"[..]),
        "a blocked export returns JSON, never a zip"
    );
    let body = body_json(resp).await;
    assert_eq!(body["gate"], "release");
    let problems = body["problems"].as_array().expect("a problem list");
    assert!(!problems.is_empty());
    for problem in problems {
        for key in ["check", "file", "location", "message", "evidence"] {
            assert!(problem.get(key).is_some(), "problem lacks {key}: {problem}");
        }
    }
    assert!(problems.iter().any(|q| q["check"] == "secrets"));
    assert!(
        !body.to_string().contains(&p.value),
        "the 422 body must never carry the value"
    );
}

#[tokio::test]
async fn export_product_success_names_the_release_checks_in_a_header() {
    if !typst_on_path() {
        eprintln!("export_product success test: skipping, typst is not on PATH");
        return;
    }
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
    state.set_last_scan(project.id.clone(), scan_report());
    let resp = crate::export_product(
        axum::extract::State(state),
        axum::extract::Path(project.id),
        request(),
    )
    .await;
    if resp.status() != axum::http::StatusCode::OK {
        // This test failed intermittently in full parallel runs with a bare "500 != 200";
        // say WHY, so the next occurrence names its cause.
        use http_body_util::BodyExt;
        let status = resp.status();
        let bytes = resp.into_body().collect().await.expect("body").to_bytes();
        panic!(
            "export_product returned {status}: {}",
            String::from_utf8_lossy(&bytes)
        );
    }
    assert_eq!(
        resp.headers()
            .get("x-camerata-release-checks")
            .and_then(|v| v.to_str().ok()),
        Some(
            "secrets, dataset set, dataset parity, row parity, severity and kind counts, \
             confidentiality marking, manifest, provenance"
        )
    );
}

// ---- the secrets check runs BEFORE the PDF compile --------------------------------------------

/// A fake `typst` that records that it ran and fails, so a test can prove the compiler was
/// (or was not) reached. Returns the directory holding it (`<dir>/typst`, handed to the
/// task-local `report_export::TYPST_PROGRAM` seam, never put on PATH) and the marker file it
/// touches.
#[cfg(unix)]
fn recording_typst() -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let marker = dir.path().join("typst-was-run");
    let script = dir.path().join("typst");
    std::fs::write(
        &script,
        format!("#!/bin/sh\n: > '{}'\nexit 1\n", marker.display()),
    )
    .expect("write the fake typst");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    (dir, marker)
}

#[cfg(unix)]
#[tokio::test]
async fn a_secret_in_the_pdf_data_blocks_the_package_before_typst_is_ever_run() {
    let (fake_bin, marker) = recording_typst();
    let fake_typst = fake_bin.path().join("typst");
    let p = planted(SecretShape::StripeLiveSecretKey);
    // The report options are free text the operator or the export dialog supplies; they reach
    // `data.json` without passing through the finding redactor.
    let options = ReportOptions {
        prepared_by: format!("Prepared with {}", p.value),
        ..client_options()
    };
    let error = crate::report_export::TYPST_PROGRAM
        .scope(
            fake_typst,
            build_product_package_with_stem(
                &sealed_report(),
                &HashMap::new(),
                None,
                &options,
                STEM,
            ),
        )
        .await
        .err()
        .expect("the package must be refused");
    let failure = gate_failure_of(&error).expect("the refusal is the gate's, not a compile error");
    assert!(
        !failure.to_string().contains(&p.value),
        "the block never carries the value"
    );
    assert!(
        failure.to_string().contains("sk_live_\u{2026}"),
        "{failure}"
    );
    assert!(
        !marker.exists(),
        "typst ran: the raw data.json reached a compile tempdir"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_clean_package_still_reaches_the_compiler() {
    let (fake_bin, marker) = recording_typst();
    let fake_typst = fake_bin.path().join("typst");
    let error = crate::report_export::TYPST_PROGRAM
        .scope(
            fake_typst,
            build_product_package_with_stem(
                &sealed_report(),
                &HashMap::new(),
                None,
                &client_options(),
                STEM,
            ),
        )
        .await
        .err()
        .expect("the fake typst fails the compile");
    assert!(gate_failure_of(&error).is_none(), "{error}");
    assert!(marker.exists(), "a clean package is compiled");
}

/// Confidentiality (2026-10-24 decision): a blank client name blocks the export with a clear
/// message, before Typst runs, so no deliverable says "prepared for .".
#[cfg(unix)]
#[tokio::test]
#[serial_test::serial(process_env)]
async fn a_blank_client_name_blocks_the_package_before_typst_is_ever_run() {
    let (fake_bin, marker) = recording_typst();
    let _path = camerata_test_env::EnvGuard::set("PATH", fake_bin.path());
    for blank in ["", "   "] {
        let options = ReportOptions {
            client_name: blank.to_string(),
            ..ReportOptions::default()
        };
        let error = build_product_package_with_stem(
            &sealed_report(),
            &HashMap::new(),
            None,
            &options,
            STEM,
        )
        .await
        .err()
        .expect("a blank client name must block the export");
        let refusal = error
            .downcast_ref::<crate::report_export::ClientNameRequired>()
            .expect("the refusal names the missing client name");
        assert!(
            refusal.to_string().contains("client name"),
            "the message says what to fix: {refusal}"
        );
    }
    assert!(!marker.exists(), "typst ran for a package with no client");
}

// ---- regression guard: the seed text a client reads must never false-block the gate ------------

/// The corpus fields that can reach a client (the set the W25 client-surface test walks): the
/// rule title, every source title, and per option the directive, remediation, label, finding
/// headline and detail, plus a `violates` option's `why` (interpolated into the defect detail).
/// Each is `(location, text)`.
fn client_facing_corpus_text(corpus: &camerata_rules::RuleSet) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for rule in corpus.iter() {
        let id = &rule.id.0;
        out.push((format!("{id} title"), rule.title.clone()));
        for source in &rule.sources {
            out.push((format!("{id} source title"), source.title.clone()));
        }
        for option in &rule.options {
            let oid = &option.id;
            out.push((format!("{id} {oid}.directive"), option.directive.clone()));
            out.push((format!("{id} {oid}.label"), option.label.clone()));
            for (field, text) in [
                ("remediation", option.remediation.as_ref()),
                ("finding_headline", option.finding_headline.as_ref()),
                ("finding_detail", option.finding_detail.as_ref()),
            ] {
                if let Some(text) = text {
                    out.push((format!("{id} {oid}.{field}"), text.clone()));
                }
            }
            if option.violates {
                out.push((format!("{id} {oid}.why"), option.why.clone()));
            }
        }
    }
    out
}

#[tokio::test]
async fn the_gate_raises_no_problem_on_the_seed_text_every_package_ships() {
    let (corpus, errors) =
        camerata_rules::load_corpus_lenient(&camerata_rules::corpus_path()).await;
    assert!(
        errors.is_empty(),
        "the corpus must load cleanly: {errors:?}"
    );
    let seed = client_facing_corpus_text(&corpus);
    assert!(
        seed.len() > 500,
        "the walk must actually see the corpus ({})",
        seed.len()
    );

    let mut problems = Vec::new();
    for (location, text) in &seed {
        problems.extend(secrets::text_problems(location, text));
    }
    // The Typst template ships in every PDF's pipeline and is read into every package.
    problems.extend(secrets::text_problems(
        "templates/audit_report.typ",
        include_str!("../../templates/audit_report.typ"),
    ));
    // Report only where and which shape (the evidence is already the redacted form).
    let shown: Vec<String> = problems
        .iter()
        .map(|p| format!("{} [{}] {} {:?}", p.file, p.location, p.message, p.evidence))
        .collect();
    assert!(
        problems.is_empty(),
        "seed text false-blocks the release gate ({}):\n  {}",
        problems.len(),
        shown.join("\n  ")
    );
}

// ---- confidentiality: the client name (2026-10-24 decision) -------------------------------------

/// Both export routes refuse a project whose inspection setup has no client name, with a 422
/// that says where to enter it, before anything is compiled (no Typst needed).
#[tokio::test]
async fn both_export_routes_refuse_a_project_with_no_client_name() {
    for route in ["pdf", "zip"] {
        let state = crate::AppState::new(std::sync::Arc::new(crate::InMemoryStoryStore::new()));
        let project = state
            .projects
            .create("Acme", vec!["acme/portal".to_string()])
            .expect("project created");
        state.set_last_scan(project.id.clone(), scan_report());
        let resp = if route == "pdf" {
            crate::export_audit_report(
                axum::extract::State(state),
                axum::extract::Path(project.id),
                request(),
            )
            .await
        } else {
            crate::export_product(
                axum::extract::State(state),
                axum::extract::Path(project.id),
                request(),
            )
            .await
        };
        assert_eq!(
            resp.status(),
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "{route}"
        );
        let body = body_json(resp).await;
        assert_eq!(body["gate"], "client_name", "{route}: {body}");
        assert!(
            body["message"]
                .as_str()
                .is_some_and(|m| m.contains("inspection setup")),
            "{route}: the message says where to enter the name: {body}"
        );
    }
}

/// The setter saves a trimmed name on the project, and refuses one that spans lines or is too
/// long for a footer, leaving the saved name as it was.
#[tokio::test]
async fn the_client_name_setter_saves_a_valid_name_and_refuses_a_bad_one() {
    let state = crate::AppState::new(std::sync::Arc::new(crate::InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Acme", vec!["acme/portal".to_string()])
        .expect("project created");
    let set = |name: String| {
        crate::set_client_name(
            axum::extract::State(state.clone()),
            axum::extract::Path(project.id.clone()),
            axum::Json(crate::SetClientNameReq { client_name: name }),
        )
    };
    let ok = set("  Example Client Ltd ".to_string()).await;
    assert_eq!(ok.0["ok"], true);
    assert_eq!(
        state
            .projects
            .get(&project.id)
            .expect("project")
            .client_name,
        "Example Client Ltd"
    );
    for bad in [
        "Example\nClient".to_string(),
        "x".repeat(crate::report_export::CLIENT_NAME_MAX_CHARS + 1),
    ] {
        let refused = set(bad).await;
        assert_eq!(refused.0["ok"], false);
        assert!(refused.0["message"].as_str().is_some_and(|m| !m.is_empty()));
    }
    assert_eq!(
        state
            .projects
            .get(&project.id)
            .expect("project")
            .client_name,
        "Example Client Ltd",
        "a refused name never replaces the saved one"
    );
}

/// The marking reaches every workbook sheet's footer, it is the PDF's line, and the gate
/// refuses a workbook whose sheets lost it.
#[test]
fn every_sheet_footer_carries_the_pdfs_confidentiality_line_and_the_gate_checks_it() {
    let clean = package();
    let line = clean.report_json.confidentiality.line.clone();
    assert!(
        line.starts_with("Confidential, prepared for Example Client Ltd on "),
        "{line}"
    );
    let footers = crate::release_gate::xlsx_header_footers(&clean.xlsx).expect("footers");
    let sheets = crate::release_gate::xlsx_sheet_names(&clean.xlsx).expect("sheets");
    assert!(
        sheets.len() > 3,
        "anti-vacuity: the workbook has many sheets"
    );
    for sheet in &sheets {
        let footer = footers
            .iter()
            .find(|f| f.part == "footer" && &f.sheet == sheet)
            .unwrap_or_else(|| panic!("sheet {sheet:?} has no footer"));
        assert_eq!(footer.left, line, "sheet {sheet:?}");
    }

    // A workbook with no footers (an older build, or a sheet writer that dropped them) is
    // refused by the archive pass alone.
    let mut stripped = package();
    let entries_without = {
        let sealed = sealed_report();
        let options = client_options();
        let mut wb = crate::xlsx_export::build_workbook(&sealed, &HashMap::new(), None, &options)
            .expect("workbook");
        // Rewrite every `<oddFooter>` out of the workbook's sheet XML.
        wb = rewrite_xlsx(&wb, |name, xml| {
            if name.starts_with("xl/worksheets/sheet") {
                let mut out = xml.to_string();
                while let (Some(a), Some(b)) = (out.find("<oddFooter>"), out.find("</oddFooter>")) {
                    out.replace_range(a..b + "</oddFooter>".len(), "");
                }
                out
            } else {
                xml.to_string()
            }
        });
        wb
    };
    stripped.xlsx = entries_without;
    let error = release(stripped).expect_err("a workbook without the marking must be refused");
    let ReleaseError::Blocked(failure) = error else {
        panic!("expected a gate refusal, got {error}");
    };
    assert!(
        failure
            .report
            .problems()
            .any(|(check, p)| check == CheckId::Marking
                && p.message.contains("no confidentiality footer")),
        "{failure}"
    );
}

/// Rebuild an xlsx (a zip) with each UTF-8 member passed through `f`.
fn rewrite_xlsx(xlsx: &[u8], f: impl Fn(&str, &str) -> String) -> Vec<u8> {
    let entries = read_zip_entries(xlsx).expect("the workbook is a zip");
    let mut out = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in entries {
        out.start_file(name.as_str(), options)
            .expect("start member");
        match std::str::from_utf8(&bytes) {
            Ok(text) => out.write_all(f(&name, text).as_bytes()),
            Err(_) => out.write_all(&bytes),
        }
        .expect("write member");
    }
    out.finish().expect("finish zip").into_inner()
}

/// The client is named by one string everywhere. A name typed with invisible characters in it
/// used to reach the PDF without them and the manifest with them, so the same package named the
/// client two ways.
#[test]
fn the_manifest_the_pdf_and_the_workbook_name_the_client_identically() {
    let sealed = sealed_report();
    let options = ReportOptions {
        client_name: " Acme\u{200B} Ltd\u{FEFF} ".to_string(),
        ..ReportOptions::default()
    };
    let json = build_report_json(&sealed, &HashMap::new(), None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        &sealed,
        &HashMap::new(),
        None,
        &options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles");

    assert_eq!(package.report_json.cover.client_name, "Acme Ltd");
    assert!(package
        .report_json
        .confidentiality
        .line
        .contains("prepared for Acme Ltd on "));
    assert_eq!(package.provenance.client_name.as_deref(), Some("Acme Ltd"));
    let index = archive::xlsx_sheet_grid(&package.xlsx, "Index").expect("the Index sheet");
    assert!(
        index
            .iter()
            .any(|row| row.first().is_some_and(|c| c == "Prepared for Acme Ltd")),
        "the workbook Index names the client the same way"
    );
}

/// The marking goes through the release gate like every other string: a secret-shaped value
/// typed into the client name is caught in the workbook's sheet footers too, not only in the
/// PDF's data.
#[test]
fn a_secret_in_the_client_name_is_caught_in_the_sheet_footers() {
    let p = planted(SecretShape::StripeLiveSecretKey);
    let sealed = sealed_report();
    let options = ReportOptions {
        client_name: format!("Acme {}", p.value),
        ..ReportOptions::default()
    };
    let json = build_report_json(&sealed, &HashMap::new(), None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        &sealed,
        &HashMap::new(),
        None,
        &options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles");
    let error = release(package).expect_err("the package must be refused");
    let ReleaseError::Blocked(failure) = error else {
        panic!("expected a gate refusal, got {error}");
    };
    assert!(
        failure
            .report
            .problems()
            .any(|(check, q)| check == CheckId::Secrets && q.location.ends_with(" footer")),
        "{failure}"
    );
    assert!(!failure.to_string().contains(&p.value));
}

// ---- 22d: severity and kind counts reconcile across PDF, workbook, JSON and CSV -----------------

/// A scan with every kind and several severities, so a by-severity-and-kind tally has cells to
/// disagree in (synthetic).
fn mixed_scan_report() -> ScanReport {
    use camerata_api_types::audit::{
        ConventionObservedKind, DeviationKind, FindingKind, NoConventionKind, NotAssessedKind,
        NotAssessedReason,
    };
    let deviation = || {
        FindingKind::Deviation(DeviationKind {
            deviating_sites: 2,
            assessed_sites: Some(10),
            convention_option_id: Some("versioned".to_string()),
            convention_label: "URI-versioned endpoints".to_string(),
        })
    };
    let rows: Vec<(&str, &str, Option<FindingKind>)> = vec![
        (
            "SEC-NO-RAW-SQL-CONCAT-1",
            "critical",
            Some(FindingKind::Defect),
        ),
        (
            "SEC-NO-HARDCODED-SECRETS-1",
            "high",
            Some(FindingKind::Defect),
        ),
        ("SEC-NO-DISABLED-TLS-1", "high", None),
        ("API-VERSIONING-1", "medium", Some(deviation())),
        ("API-VERSIONING-1", "medium", Some(deviation())),
        ("API-PAGINATION-1", "low", Some(FindingKind::Defect)),
        (
            "API-CURSOR-1",
            "info",
            Some(FindingKind::ConventionObserved(ConventionObservedKind {
                convention_option_id: "cursor".to_string(),
                convention_label: "Cursor pagination".to_string(),
            })),
        ),
        (
            "API-ERRORS-1",
            "info",
            Some(FindingKind::NoConvention(NoConventionKind {
                observed_options: Vec::new(),
            })),
        ),
        (
            "API-TIMEOUTS-1",
            "info",
            Some(FindingKind::NotAssessed(NotAssessedKind {
                reason: NotAssessedReason::NoFilesMatched,
                reason_detail: "no file matched the rule's scope".to_string(),
            })),
        ),
    ];
    let mut report = scan_report();
    report.findings = rows
        .into_iter()
        .enumerate()
        .map(|(i, (rule, severity, kind))| Finding {
            repo: "acme/portal".to_string(),
            path: format!("src/mod{i}.rs"),
            line: i + 1,
            rule_id: rule.to_string(),
            severity: severity.to_string(),
            snippet: format!("let x{i} = 1;"),
            detail: format!("detail {i}"),
            kind,
            ..Finding::default()
        })
        .collect();
    report
}

fn mixed_package() -> ProductPackage {
    let sealed = mixed_scan_report().seal();
    let options = client_options();
    let json = build_report_json(&sealed, &HashMap::new(), None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &sealed,
        &HashMap::new(),
        None,
        &options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles")
}

fn count_problems(failure: &GateFailure) -> Vec<&Problem> {
    failure
        .report
        .problems()
        .filter(|(check, _)| *check == CheckId::Counts)
        .map(|(_, p)| p)
        .collect()
}

#[test]
fn a_package_with_every_kind_reconciles_by_severity_and_kind() {
    let package = mixed_package();
    // Anti-vacuity: the JSON rows really span several kinds and severities.
    let rows: Vec<Value> = serde_json::to_value(&package.findings_export).expect("rows")
        ["findings"]
        .as_array()
        .expect("rows")
        .clone();
    let kinds: std::collections::BTreeSet<&str> =
        rows.iter().filter_map(|r| r["kind"].as_str()).collect();
    let severities: std::collections::BTreeSet<&str> =
        rows.iter().filter_map(|r| r["severity"].as_str()).collect();
    assert!(kinds.len() >= 5, "{kinds:?}");
    assert!(severities.len() >= 4, "{severities:?}");
    let released = release(package).unwrap_or_else(|e| panic!("{e}"));
    assert!(released
        .report
        .check_names()
        .contains(&"severity and kind counts"));
}

/// A clean codebase is the best case the product can report, and its tables have no rows. An empty
/// table has no header to read the severity column from, so the reconciliation reads it as zero
/// rows instead of refusing the export.
#[test]
fn a_scan_with_no_findings_reconciles_and_releases() {
    let mut report = scan_report();
    report.findings.clear();
    let sealed = report.seal();
    let options = client_options();
    let json = build_report_json(&sealed, &HashMap::new(), None, &options);
    let data = render_data_json(&json).expect("data.json renders");
    let package = assemble_package(
        &sealed,
        &HashMap::new(),
        None,
        &options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles");
    let released = release(package).unwrap_or_else(|e| panic!("{e}"));
    assert!(released
        .report
        .check_names()
        .contains(&"severity and kind counts"));
}

/// An empty CSV is still checked against its JSON: rows in the JSON and none in the CSV is a
/// disagreement, not a pass.
#[test]
fn an_emptied_csv_is_still_held_to_the_json_rows() {
    let mut package = mixed_package();
    for (path, bytes) in package.datasets.iter_mut() {
        if path == "csv/all-findings.csv" {
            bytes.clear();
        }
    }
    let failure = blocked(package);
    assert!(
        count_problems(&failure)
            .iter()
            .any(|p| p.file == "csv/all-findings.csv" && p.location.starts_with("severity ")),
        "{failure}"
    );
}

/// The kind dimension is checked, not only totals: a PDF row relabelled from defect to deviation
/// keeps every severity total equal and is still caught.
#[test]
fn the_gate_catches_a_pdf_whose_kinds_differ_even_when_every_total_matches() {
    let mut package = mixed_package();
    let row = [
        &mut package.report_json.matrix.do_now,
        &mut package.report_json.matrix.do_next,
        &mut package.report_json.matrix.plan,
        &mut package.report_json.matrix.accepted,
        &mut package.report_json.matrix.informational,
        &mut package.report_json.matrix.held,
    ]
    .into_iter()
    .flat_map(|bucket| bucket.iter_mut())
    .find(|f| f.kind.tag() == "defect")
    .expect("a defect row in the PDF");
    row.kind = camerata_api_types::audit::KindOnWire(Some(
        camerata_api_types::audit::FindingKind::Deviation(
            camerata_api_types::audit::DeviationKind {
                deviating_sites: 1,
                assessed_sites: None,
                convention_option_id: None,
                convention_label: "x".to_string(),
            },
        ),
    ));
    let failure = blocked(package);
    let problems = count_problems(&failure);
    assert!(
        problems
            .iter()
            .any(|p| p.file == "PDF data" && p.location.ends_with("kind deviation")),
        "{failure}"
    );
    assert!(
        !problems
            .iter()
            .any(|p| p.location.starts_with("cover.stats")),
        "every severity total still matches: {failure}"
    );
}

/// The groups the PDF renders as finding cards: "Curated findings" and "Held for review".
fn rendered_groups(
    package: &mut ProductPackage,
) -> impl Iterator<Item = &mut crate::report_export::CuratedGroupJson> {
    package
        .report_json
        .curated_findings
        .iter_mut()
        .chain(package.report_json.held_for_review_findings.iter_mut())
}

fn rendered_count(package: &mut ProductPackage, severity: &str, kind: &str) -> usize {
    rendered_groups(package)
        .flat_map(|g| g.sites.iter())
        .filter(|s| s.severity == severity && s.kind.tag() == kind)
        .count()
}

/// The PDF renders its findings from `curated_findings` and `held_for_review_findings`, not from
/// the matrix the counts are first taken from. A site that falls out of those lists is a finding
/// the reader never sees while the matrix, the cover and every total still agree.
#[test]
fn the_gate_catches_a_finding_the_pdf_counts_but_does_not_render() {
    let mut package = mixed_package();
    // Anti-vacuity: the critical defect really is rendered once.
    assert_eq!(rendered_count(&mut package, "critical", "defect"), 1);
    for group in rendered_groups(&mut package) {
        group
            .sites
            .retain(|s| !(s.severity == "critical" && s.kind.tag() == "defect"));
    }
    let failure = blocked(package);
    assert!(
        count_problems(&failure).iter().any(|p| p.file == "PDF data"
            && p.location == "severity critical, kind defect"
            && p.message.contains("rendered")),
        "{failure}"
    );
}

#[test]
fn the_gate_catches_a_rendered_finding_whose_kind_differs_from_the_matrix() {
    let mut package = mixed_package();
    let site = rendered_groups(&mut package)
        .flat_map(|g| g.sites.iter_mut())
        .find(|s| s.kind.tag() == "defect")
        .expect("a rendered defect");
    site.kind = camerata_api_types::audit::KindOnWire(Some(
        camerata_api_types::audit::FindingKind::Deviation(
            camerata_api_types::audit::DeviationKind {
                deviating_sites: 1,
                assessed_sites: None,
                convention_option_id: None,
                convention_label: "x".to_string(),
            },
        ),
    ));
    let failure = blocked(package);
    assert!(
        count_problems(&failure)
            .iter()
            .any(|p| p.file == "PDF data" && p.message.contains("rendered")),
        "{failure}"
    );
}

#[test]
fn the_gate_catches_a_finding_rendered_twice() {
    let mut package = mixed_package();
    let group = rendered_groups(&mut package)
        .find(|g| !g.sites.is_empty())
        .expect("a rendered group");
    let again = group.sites[0].clone();
    group.sites.push(again);
    let failure = blocked(package);
    assert!(
        count_problems(&failure)
            .iter()
            .any(|p| p.file == "PDF data" && p.message.contains("rendered")),
        "{failure}"
    );
}

#[test]
fn the_gate_catches_a_pdf_summary_count_that_is_off_by_one() {
    let mut package = mixed_package();
    package.report_json.cover.stats.high += 1;
    let failure = blocked(package);
    assert!(
        count_problems(&failure)
            .iter()
            .any(|p| p.location == "cover.stats.high"),
        "{failure}"
    );
}

#[test]
fn the_gate_catches_a_workbook_whose_rows_disagree_by_severity() {
    let mut package = mixed_package();
    // Every "Critical" severity cell now reads "Medium" (the cells share one string).
    package.xlsx = rewrite_xlsx(&package.xlsx, |name, xml| {
        if name == "xl/sharedStrings.xml" {
            xml.replace("<t>Critical</t>", "<t>Medium</t>")
        } else {
            xml.to_string()
        }
    });
    let failure = blocked(package);
    let problems = count_problems(&failure);
    assert!(
        problems.iter().any(|p| p.file.ends_with("-findings.xlsx")
            && p.location == "severity critical, kind defect"),
        "{failure}"
    );
}

// ---- 22d: the Conventions and Coverage sheets' rows are tallied too ----------------------------

/// The position of `text` in the workbook's shared-string table (each distinct cell text is
/// stored once; a cell holds the position).
fn shared_string_index(xlsx: &[u8], text: &str) -> usize {
    let entries = read_zip_entries(xlsx).expect("the workbook is a zip");
    let table = String::from_utf8(
        entries
            .get("xl/sharedStrings.xml")
            .expect("a shared string table")
            .clone(),
    )
    .expect("utf8");
    table
        .split("<si>")
        .skip(1)
        .position(|item| item.contains(&format!(">{text}</t>")))
        .unwrap_or_else(|| panic!("the workbook has no cell text {text:?}"))
}

/// Remove every worksheet row that holds a cell with exactly this text, leaving the rest of the
/// workbook (its Index counts included) untouched: a row dropped from one sheet by itself.
fn drop_sheet_rows_with_text(xlsx: &[u8], text: &str) -> Vec<u8> {
    let cell = format!("t=\"s\"><v>{}</v>", shared_string_index(xlsx, text));
    rewrite_xlsx(xlsx, |name, xml| {
        if !(name.starts_with("xl/worksheets/sheet") && name.ends_with(".xml")) {
            return xml.to_string();
        }
        let mut out = String::new();
        let mut rest = xml;
        while let Some(start) = rest.find("<row ") {
            let end = rest[start..]
                .find("</row>")
                .map(|e| start + e + "</row>".len())
                .expect("a closed row");
            out.push_str(&rest[..start]);
            if !rest[start..end].contains(&cell) {
                out.push_str(&rest[start..end]);
            }
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    })
}

#[test]
fn the_gate_catches_a_no_convention_row_dropped_from_the_conventions_sheet() {
    let mut package = mixed_package();
    package.xlsx = drop_sheet_rows_with_text(&package.xlsx, "No single convention adopted");
    let failure = blocked(package);
    let problems = count_problems(&failure);
    assert!(
        problems.iter().any(|p| p.file.ends_with("-findings.xlsx")
            && p.location == "sheet \"Conventions\" rows, kind no_convention"),
        "{failure}"
    );
    assert!(
        !problems
            .iter()
            .any(|p| p.location.contains("kind convention_observed")),
        "only the dropped kind is named: {failure}"
    );
}

#[test]
fn the_gate_catches_a_not_assessed_row_dropped_from_the_coverage_sheet() {
    let mut package = mixed_package();
    package.xlsx = drop_sheet_rows_with_text(&package.xlsx, "API-TIMEOUTS-1");
    let failure = blocked(package);
    assert!(
        count_problems(&failure)
            .iter()
            .any(|p| p.file.ends_with("-findings.xlsx")
                && p.location == "sheet \"Coverage\" rows, kind not_assessed"),
        "{failure}"
    );
}

#[test]
fn the_gate_catches_a_followed_convention_row_dropped_from_the_conventions_sheet() {
    let mut package = mixed_package();
    package.xlsx = drop_sheet_rows_with_text(
        &package.xlsx,
        "Followed consistently \u{2014} no exceptions found",
    );
    let failure = blocked(package);
    assert!(
        count_problems(&failure)
            .iter()
            .any(|p| p.location == "sheet \"Conventions\" rows, kind convention_observed"),
        "{failure}"
    );
}
