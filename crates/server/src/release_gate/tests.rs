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

fn scan_report() -> ScanReport {
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
            started_at: "2026-10-21T00:00:00Z".to_string(),
            finished_at: "2026-10-21T00:05:00Z".to_string(),
        },
        recommendations: HashMap::new(),
        failed_passes: Vec::new(),
        ledger: crate::scan_ledger::ScanLedger::new(),
    }
}

/// A clean package, built by the production assembly around a stand-in PDF.
fn package() -> ProductPackage {
    let sealed = sealed_report();
    let options = ReportOptions::default();
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

fn blocked(package: ProductPackage) -> GateFailure {
    match release(package) {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("expected the gate to block, got {other}"),
        Ok(_) => panic!("expected the gate to block, but the package was released"),
    }
}

// ---- planting a secret on each client-visible surface --------------------------------------

#[derive(Debug, Clone, Copy)]
enum Surface {
    DataJson,
    FindingsJson,
    DatasetJson,
    DatasetCsv,
    XlsxSharedString,
    Readme,
    Stem,
}

const ALL_SURFACES: [Surface; 7] = [
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
fn plant(package: &mut ProductPackage, surface: Surface, text: &str) {
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
fn release_passes_a_clean_package_and_reports_six_checks() {
    let z = released();
    assert!(z.report.is_clean());
    assert_eq!(
        z.report.check_names(),
        vec![
            "secrets",
            "dataset set",
            "dataset parity",
            "row parity",
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
            "Release checks: secrets, dataset set, dataset parity, row parity, manifest, \
             provenance (all passed)"
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
        5,
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
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some()
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
    state.set_last_scan(project.id.clone(), scan_report());
    let resp = crate::export_product(
        axum::extract::State(state),
        axum::extract::Path(project.id),
        request(),
    )
    .await;
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("x-camerata-release-checks")
            .and_then(|v| v.to_str().ok()),
        Some("secrets, dataset set, dataset parity, row parity, manifest, provenance")
    );
}

// ---- the secrets check runs BEFORE the PDF compile --------------------------------------------

/// A `typst` on PATH that records that it ran and fails, so a test can prove the compiler was
/// (or was not) reached. Returns the directory to put on PATH and the marker file it touches.
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
#[serial_test::serial(process_env)]
async fn a_secret_in_the_pdf_data_blocks_the_package_before_typst_is_ever_run() {
    let (fake_bin, marker) = recording_typst();
    let _path = camerata_test_env::EnvGuard::set("PATH", fake_bin.path());
    let p = planted(SecretShape::StripeLiveSecretKey);
    // The report options are free text the operator or the export dialog supplies; they reach
    // `data.json` without passing through the finding redactor.
    let options = ReportOptions {
        prepared_by: format!("Prepared with {}", p.value),
        ..ReportOptions::default()
    };
    let error =
        build_product_package_with_stem(&sealed_report(), &HashMap::new(), None, &options, STEM)
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
#[serial_test::serial(process_env)]
async fn a_clean_package_still_reaches_the_compiler() {
    let (fake_bin, marker) = recording_typst();
    let _path = camerata_test_env::EnvGuard::set("PATH", fake_bin.path());
    let error = build_product_package_with_stem(
        &sealed_report(),
        &HashMap::new(),
        None,
        &ReportOptions::default(),
        STEM,
    )
    .await
    .err()
    .expect("the fake typst fails the compile");
    assert!(gate_failure_of(&error).is_none(), "{error}");
    assert!(marker.exists(), "a clean package is compiled");
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
