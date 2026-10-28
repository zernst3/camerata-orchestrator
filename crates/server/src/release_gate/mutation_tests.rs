//! Tests written against the mutants `cargo mutants -p camerata-server --file
//! crates/server/src/release_gate.rs --file 'crates/server/src/release_gate/*.rs'` left alive on
//! 2026-10-28 (docs/MUTATION_TESTING.md). The gate decides whether an export reaches a client,
//! so each check here must be shown to fire, not only to pass a clean package.

use std::collections::BTreeMap;
use std::io::Write;

use super::*;

fn zip_of(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    for (name, bytes) in members {
        writer.start_file(*name, opts).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

// ---- the released package's record -------------------------------------------------------

#[test]
fn a_package_record_hashes_the_exact_bytes_and_keeps_the_manifest() {
    let bytes = zip_of(&[("manifest.json", br#"{"files":["README.txt"]}"#)]);
    let released = ReleasedZip {
        stem: "camerata-inspection-acme-abc123".to_string(),
        bytes: bytes.clone(),
        report: GateReport::default(),
    };
    let record = released.package_record();
    use sha2::{Digest, Sha256};
    let expected: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(record.zip_sha256, expected);
    assert_eq!(
        record.manifest_json,
        serde_json::json!({"files": ["README.txt"]})
    );
    assert!(!record.generated_at.is_empty());
}

// ---- refusal messages ----------------------------------------------------------------------

#[test]
fn a_release_error_says_what_went_wrong() {
    assert_eq!(
        ReleaseError::Assembly("disk full".to_string()).to_string(),
        "could not assemble the product export zip: disk full"
    );
    let failure = GateFailure {
        stem: "camerata-inspection-acme-abc123".to_string(),
        report: GateReport::default(),
    };
    assert_eq!(
        ReleaseError::Blocked(failure.clone()).to_string(),
        failure.to_string()
    );
    assert!(failure
        .to_string()
        .starts_with("EXPORT BLOCKED by the release gate"));
}

// ---- the provenance check --------------------------------------------------------------------

fn manifest() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "generated_at": "2026-07-23 00:05 UTC",
        "inspection": {"repos": [{"name": "owner/repo", "commit": "abc123", "dirty": false}]},
    }))
    .unwrap()
}

#[test]
fn a_readme_that_disagrees_with_the_manifest_is_a_provenance_problem() {
    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    entries.insert(
        "README.txt".to_string(),
        b"Repos inspected: someone/else\n".to_vec(),
    );
    entries.insert("manifest.json".to_string(), manifest());
    let problems = provenance_problems(&entries);
    assert!(
        problems.iter().any(|p| p.file == "README.txt"),
        "{problems:?}"
    );

    entries.insert("manifest.json".to_string(), b"not json".to_vec());
    let problems = provenance_problems(&entries);
    assert_eq!(problems.len(), 1);
    assert_eq!(problems[0].message, "manifest.json does not parse");

    entries.remove("README.txt");
    assert!(
        provenance_problems(&entries).is_empty(),
        "a missing README is the dataset-set check's to report"
    );
}

#[test]
fn audit_zip_runs_the_provenance_check() {
    let zip = zip_of(&[
        ("README.txt", b"Repos inspected: someone/else\n"),
        ("manifest.json", &manifest()),
    ]);
    let report = audit_zip(&zip, None);
    assert!(
        report
            .problems()
            .any(|(check, _)| check == CheckId::Provenance),
        "{report:?}"
    );
}

// ---- the workbook reader the gate checks with ------------------------------------------------

/// A minimal two-sheet workbook: a shared string, an inline string and a number on sheet 1, one
/// shared string on sheet 2, and a self-closing empty cell that must be skipped. `AB12` pins the
/// column arithmetic (A=0, Z=25, AA=26, AB=27) and the one-based row.
fn tiny_workbook() -> Vec<u8> {
    zip_of(&[
        (
            "xl/workbook.xml",
            br#"<workbook><sheets><sheet name="Findings" sheetId="1"/><sheet name="R&amp;D" sheetId="2"/></sheets></workbook>"#,
        ),
        (
            "xl/sharedStrings.xml",
            br#"<sst><si><t>shared &amp; text</t></si><si><t xml:space="preserve">two </t><t>runs</t></si></sst>"#,
        ),
        (
            "xl/worksheets/sheet1.xml",
            br#"<sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>inline</t></is></c><c r="B1"/></row><row r="12"><c r="AB12" t="s"><v>0</v></c><c r="C12"><v>42</v></c></row></sheetData>"#,
        ),
        (
            "xl/worksheets/sheet2.xml",
            br#"<sheetData><row r="2"><c r="B2" t="s"><v>1</v></c></row></sheetData>"#,
        ),
    ])
}

#[test]
fn the_workbook_reader_resolves_every_cell_to_its_sheet_and_position() {
    let cells: Vec<(String, String, usize, usize, String)> = xlsx_cells(&tiny_workbook())
        .unwrap()
        .into_iter()
        .map(|c| (c.sheet, c.reference, c.row, c.col, c.text))
        .collect();
    let owned = |s: &str, r: &str, row: usize, col: usize, t: &str| {
        (s.to_string(), r.to_string(), row, col, t.to_string())
    };
    assert_eq!(
        cells,
        vec![
            owned("Findings", "A1", 0, 0, "inline"),
            owned("Findings", "AB12", 11, 27, "shared & text"),
            owned("Findings", "C12", 11, 2, "42"),
            owned("R&D", "B2", 1, 1, "two runs"),
        ]
    );
}

#[test]
fn a_sheet_grid_is_dense_and_names_a_missing_sheet() {
    let grid = xlsx_sheet_grid(&tiny_workbook(), "R&D").unwrap();
    assert_eq!(grid, vec![vec!["", ""], vec!["", "two runs"]]);
    let err = xlsx_sheet_grid(&tiny_workbook(), "Nope").unwrap_err();
    assert!(err.starts_with("no worksheet \"Nope\""), "{err}");
}

// ---- dataset parity ----------------------------------------------------------------------------

#[test]
fn a_duplicated_csv_column_is_a_header_problem_even_when_the_key_sets_match() {
    let json = br#"[{"a":"1","b":"2"}]"#;
    assert!(compare_csv_to_json("all-findings", b"a,b\n1,2\n", json).is_empty());
    let problems = compare_csv_to_json("all-findings", b"a,a,b\n1,1,2\n", json);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].location, "header");
}

#[test]
fn findings_json_must_match_the_all_findings_dataset_row_for_row() {
    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    entries.insert(
        "json/all-findings.json".to_string(),
        br#"[{"rule_id":"A-1"},{"rule_id":"B-2"}]"#.to_vec(),
    );
    entries.insert(
        "findings.json".to_string(),
        br#"{"findings":[{"rule_id":"A-1"},{"rule_id":"B-2"}]}"#.to_vec(),
    );
    assert!(dataset_parity_problems(&entries).is_empty());
    entries.insert(
        "findings.json".to_string(),
        br#"{"findings":[{"rule_id":"A-1"},{"rule_id":"C-3"}]}"#.to_vec(),
    );
    let problems = dataset_parity_problems(&entries);
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].location, "findings[1]");
}

#[test]
fn every_manifest_inspection_value_must_appear_in_the_readme() {
    // A string, an array of strings and an array of repo objects, none of them in the README.
    let manifest = serde_json::json!({
        "generated_at": "2026-07-23 00:05 UTC",
        "inspection": {
            "engine_version": "9.9.9-unlisted",
            "release_checks": ["unlisted-check"],
            "repos": [{"name": "owner/unlisted", "commit": "c0ffee", "dirty": false}],
        },
    });
    let locations: Vec<String> = verify_readme_provenance("", &manifest)
        .into_iter()
        .filter(|p| p.message == "a value in manifest.json is not in the README")
        .map(|p| p.location)
        .collect();
    assert_eq!(
        locations,
        vec![
            "inspection.engine_version",
            "inspection.release_checks",
            "inspection.repos",
            "inspection.repos",
        ],
        "the repo object contributes its name and its commit"
    );
}

// ---- the secrets check's reporting -----------------------------------------------------------

fn stripe_key() -> String {
    camerata_secrets::fixtures::planted(camerata_secrets::SecretShape::StripeLiveSecretKey).value
}

#[test]
fn a_secret_in_a_text_member_is_located_by_its_one_based_line() {
    let problems =
        secrets::text_problems("README.txt", &format!("intro\nSTRIPE={}\n", stripe_key()));
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert_eq!(problems[0].location, "line 2");
    assert_eq!(
        problems[0].message,
        "Stripe live secret key present in a client file"
    );
}

#[test]
fn a_secret_wrapped_across_a_pdf_line_break_is_found_once() {
    let key = stripe_key();
    let (head, tail) = key.split_at(12);
    for broken in [format!("{head}\n{tail}"), format!("{head}\r\n{tail}")] {
        let problems = secrets::pdf_text_problems("report.pdf", &format!("Key: {broken} end"));
        assert_eq!(problems.len(), 1, "{broken:?}: {problems:?}");
    }
    // An unbroken key is found by both passes and reported once.
    let problems = secrets::pdf_text_problems("report.pdf", &format!("Key: {key} end"));
    assert_eq!(problems.len(), 1, "{problems:?}");
}

#[test]
fn the_http_problems_body_names_check_file_location_message_and_evidence() {
    let mut report = GateReport::default();
    report.record(
        CheckId::Manifest,
        vec![Problem::new(
            "manifest.json",
            "files[0]",
            "a listed file is missing",
            Some("README.txt".to_string()),
        )],
    );
    assert_eq!(
        report.problems_json(),
        serde_json::json!([{
            "check": CheckId::Manifest.label(),
            "file": "manifest.json",
            "location": "files[0]",
            "message": "a listed file is missing",
            "evidence": "README.txt",
        }])
    );
}
