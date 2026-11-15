//! THE ENFORCEMENT of Zach's ruling that fix or rewrite is the architect's decision alone: no
//! package, client or internal, carries the operator's rewrite advisory or one of its names.
//!
//! The structural guard is that the advisory's type is outside `AuditReportJson` and
//! `ProductPackage` and the export request has no advisory field. These tests pin the tripwire
//! behind it, the gate's [`CheckId::InternalNotes`] check, in the `internal_only_tests` pattern:
//! a released package (client and internal) holds neither advisory file and no deny-listed term
//! on any surface, and a term planted on each surface makes the gate refuse. A detector that
//! finds nothing proves nothing, so every surface is planted.

use camerata_api_types::rewrite_advisory::{terms, terms_in};

use super::tests::{internal_package, package, plant, rewrite_xlsx_part, Surface, ALL_SURFACES};
use super::*;

/// `(artifact, term)` for every deny-listed term across everything a package carries: the file
/// name stem, the PDF's data, every member's name and text, and every part and string of the
/// workbook.
fn hits(zip: &[u8], data_json: &str, stem: &str) -> Vec<(String, &'static str)> {
    let mut found: Vec<(String, &'static str)> = Vec::new();
    let mut scan = |artifact: String, text: &str| {
        for term in terms_in(text) {
            found.push((artifact.clone(), term));
        }
    };
    scan("zip file name stem".to_string(), stem);
    scan("PDF data (data.json)".to_string(), data_json);
    let entries = read_zip_entries(zip).expect("the released package is a zip");
    for (name, bytes) in &entries {
        scan(format!("member name {name}"), name);
        if name.ends_with(".xlsx") {
            let parts = read_zip_entries(bytes).expect("the workbook is a zip");
            for (part, part_bytes) in &parts {
                scan(
                    format!("{name} > {part}"),
                    &String::from_utf8_lossy(part_bytes),
                );
            }
            for cell in xlsx_strings(bytes).expect("the workbook reads") {
                scan(format!("{name} (cell text)"), &cell);
            }
        } else {
            scan(name.clone(), &String::from_utf8_lossy(bytes));
        }
    }
    found
}

fn internal_notes_problems(failure: &GateFailure) -> Vec<&Problem> {
    failure
        .report
        .checks
        .iter()
        .filter(|o| o.check == CheckId::InternalNotes)
        .flat_map(|o| o.problems.iter())
        .collect()
}

fn refused(result: Result<ReleasedZip, ReleaseError>, what: &str) -> GateFailure {
    match result {
        Err(ReleaseError::Blocked(failure)) => failure,
        Err(other) => panic!("{what}: expected the gate to block, got {other}"),
        Ok(_) => panic!("{what}: a package carrying an operator-only name was released"),
    }
}

/// THE TEST: a released client package and a released internal package hold neither advisory
/// file and no deny-listed term anywhere, and the gate ran the check on both.
#[test]
fn released_zips_client_and_internal_hold_neither_file_and_no_term() {
    for (what, pkg, internal) in [
        ("client", package(), false),
        ("internal", internal_package(), true),
        (
            "internal, incomplete run",
            super::run_complete_tests::internal_incomplete_package(),
            true,
        ),
    ] {
        let data_json = pkg.data_json.clone();
        let stem = pkg.stem.clone();
        let released = if internal {
            release_internal(pkg)
        } else {
            release(pkg)
        }
        .unwrap_or_else(|e| panic!("{what}: a clean package clears the gate: {e}"));
        assert!(
            released
                .report
                .checks
                .iter()
                .any(|o| o.check == CheckId::InternalNotes),
            "{what}: the check ran"
        );
        let entries = read_zip_entries(&released.bytes).expect("zip");
        assert!(
            !entries.keys().any(|n| n.contains("rewrite_advisory")),
            "{what}: an advisory file is in the zip: {:?}",
            entries.keys().collect::<Vec<_>>()
        );
        let found = hits(&released.bytes, &data_json, &stem);
        assert!(found.is_empty(), "{what}: {found:?}");
    }
}

/// A deny-listed term planted on each surface of a client package makes the gate refuse under
/// the internal-notes check, naming the file it is in. One key and one phrase per surface, the
/// phrase in another case, since terms match ignoring case.
#[test]
fn a_planted_term_on_every_surface_makes_the_gate_refuse() {
    for surface in ALL_SURFACES {
        for planted in ["structural_share", "WORTH A CONVERSATION: fix or rewrite"] {
            let mut pkg = package();
            plant(&mut pkg, surface, planted);
            let failure = refused(release(pkg), &format!("{surface:?} {planted}"));
            let problems = internal_notes_problems(&failure);
            assert!(
                !problems.is_empty(),
                "{surface:?}: planted {planted:?}, refused for something else: {failure}"
            );
            let file = match surface {
                Surface::DataJson => "PDF data (data.json)",
                Surface::FindingsJson => "findings.json",
                Surface::DatasetJson => "json/deviations.json",
                Surface::DatasetCsv => "csv/conventions.csv",
                Surface::XlsxSharedString => "acme-inspection-findings.xlsx",
                Surface::Readme => "README.txt",
                Surface::Stem => "(archive member name)",
            };
            assert!(
                problems.iter().any(|p| p.file == file),
                "{surface:?}: no problem in {file}: {problems:?}"
            );
        }
    }
}

/// Owner call: a client whose own code holds a deny-listed term gets the export refused, with no
/// override (an override would be a leak path). The refusal names the term and the member it was
/// found in, so the operator can see why.
#[test]
fn a_term_in_the_clients_own_code_refuses_the_export_and_names_the_term_and_member() {
    let mut pkg = package();
    plant(
        &mut pkg,
        Surface::DatasetJson,
        "let structural_share = compute(/Users/auditor/src);",
    );
    let failure = refused(release(pkg), "client code");
    let shown = failure.to_string();
    assert!(shown.contains("EXPORT BLOCKED"), "{shown}");
    assert!(shown.contains("structural_share"), "{shown}");
    assert!(shown.contains("json/deviations.json"), "{shown}");
    assert!(shown.contains("file text"), "{shown}");
    // No override: the same package is refused again, however it is asked for.
    let mut again = package();
    plant(&mut again, Surface::DatasetJson, "structural_share");
    assert!(release_internal(again).is_err());
}

/// The operator's own internal package is held to it too: "never in any zip, internal included".
#[test]
fn an_internal_package_with_a_planted_term_is_refused_too() {
    for surface in [Surface::FindingsJson, Surface::Readme, Surface::DataJson] {
        let mut pkg = internal_package();
        plant(&mut pkg, surface, "Rewrite advisory (internal)");
        let failure = refused(release_internal(pkg), &format!("internal {surface:?}"));
        assert!(
            !internal_notes_problems(&failure).is_empty(),
            "{surface:?}: {failure}"
        );
    }
}

/// Every deny-listed term, not only the two above, is found where it is planted.
#[test]
fn every_term_is_refused() {
    for term in terms() {
        let mut pkg = package();
        plant(&mut pkg, Surface::FindingsJson, term);
        let failure = refused(release(pkg), term);
        assert!(
            internal_notes_problems(&failure)
                .iter()
                .any(|p| p.evidence.as_deref() == Some(term)),
            "{term}: {failure}"
        );
    }
}

/// A workbook part other than the shared strings (the document properties) is read too.
#[test]
fn a_term_in_any_workbook_part_is_refused() {
    let mut pkg = package();
    pkg.xlsx = rewrite_xlsx_part(&pkg.xlsx, "docProps/core.xml", |xml| {
        xml.replacen(
            "</cp:coreProperties>",
            "<dc:subject>repeat_cause_share</dc:subject></cp:coreProperties>",
            1,
        )
    });
    let failure = refused(release(pkg), "docProps");
    assert!(
        internal_notes_problems(&failure)
            .iter()
            .any(|p| p.location == "workbook part docProps/core.xml"),
        "{failure}"
    );
}

/// A rich-text shared string can split a phrase across `<r>` runs, so no workbook part holds it
/// whole; only the cell-text pass, which joins the runs, can find it.
#[test]
fn a_phrase_split_across_rich_text_runs_in_the_workbook_is_refused() {
    let mut pkg = package();
    pkg.xlsx = rewrite_xlsx_part(&pkg.xlsx, "xl/sharedStrings.xml", |xml| {
        xml.replacen(
            "</sst>",
            "<si><r><t xml:space=\"preserve\">worth a </t></r><r><t>conversation</t></r></si></sst>",
            1,
        )
    });
    let failure = refused(release(pkg), "rich-text runs");
    let problems = internal_notes_problems(&failure);
    assert!(
        problems
            .iter()
            .any(|p| p.location.starts_with("shared string #")
                && p.evidence.as_deref() == Some("worth a conversation")),
        "{problems:?}"
    );
}

/// The PDF's text: the caller's own extraction, the bytes of a stand-in, and a PDF that cannot
/// be read, which is refused because it could not be checked.
#[test]
fn the_pdf_text_is_read_and_an_unreadable_pdf_is_refused() {
    let pkg = package();
    let released = release(pkg).expect("clean");
    let report = audit_zip(&released.bytes, Some("Page 3: worth a conversation."));
    let problems: Vec<&Problem> = report
        .checks
        .iter()
        .filter(|o| o.check == CheckId::InternalNotes)
        .flat_map(|o| o.problems.iter())
        .collect();
    assert!(
        problems.iter().any(|p| p.location == "PDF text"),
        "{problems:?}"
    );

    // A phrase wrapped across two lines of the PDF's text is still the phrase.
    let wrapped = audit_zip(&released.bytes, Some("it is worth a\nconversation"));
    assert!(
        wrapped
            .checks
            .iter()
            .any(|o| o.check == CheckId::InternalNotes && !o.problems.is_empty()),
        "a wrapped phrase passed the gate: {wrapped:?}"
    );

    let mut stand_in = package();
    stand_in.pdf = b"%PDF-1.7 stand-in with fix_hours_per_kloc in it".to_vec();
    let failure = refused(release(stand_in), "stand-in PDF");
    assert!(
        internal_notes_problems(&failure)
            .iter()
            .any(|p| p.file.ends_with(".pdf")),
        "{failure}"
    );

    let mut unreadable = internal_package();
    let mut bytes = b"%PDF-1.7\n".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 4000));
    bytes.extend_from_slice(b"\n%%EOF\n");
    unreadable.pdf = bytes;
    // An internal package skips the client text-style check, so only this check can refuse it.
    let failure = refused(release_internal(unreadable), "unreadable PDF");
    assert!(
        internal_notes_problems(&failure)
            .iter()
            .any(|p| p.message.contains("could not be read")),
        "{failure}"
    );
}

/// Owner ruling: the gate does not ban the bare word, so a client's code and an ordinary fix can
/// say it.
#[test]
fn the_bare_word_rewrite_does_not_refuse() {
    for surface in ALL_SURFACES {
        let mut pkg = package();
        plant(
            &mut pkg,
            surface,
            "Rewrite the query with a bound parameter",
        );
        let result = release(pkg);
        assert!(
            result.is_ok(),
            "{surface:?}: {}",
            result.err().map(|e| e.to_string()).unwrap_or_default()
        );
    }
}

/// The PDF-only route's data is held to it too.
#[test]
fn the_pdf_only_route_refuses_a_term_in_its_data() {
    let data = serde_json::json!({ "note": "unsettled_areas" }).to_string();
    let failure = check_pdf_data("camerata-x", &data).expect_err("refused");
    assert!(!internal_notes_problems(&failure).is_empty(), "{failure}");
    assert!(check_pdf_data("camerata-x", r#"{"note":"Rewrite it."}"#).is_ok());
}

/// The export request carries no advisory field: the report options a client export is built
/// from (the request's `options`) have no field of the advisory's, by name.
#[test]
fn the_export_options_carry_no_advisory_field() {
    let fields = format!("{:?}", super::tests::client_options());
    assert!(
        fields.contains("project_title"),
        "the field names are shown: {fields}"
    );
    assert!(terms_in(&fields).is_empty(), "{fields}");
}

/// A real compiled PDF is read page by page: a term in its text is refused at its page even when
/// the package's own data is clean. Skipped when Typst is not installed.
#[tokio::test]
async fn a_term_in_a_compiled_pdfs_text_is_refused_at_its_page() {
    if !super::tests::typst_on_path() {
        eprintln!("skipping: typst is not on PATH");
        return;
    }
    // The control: the same package with its own data compiled is released.
    let mut clean = package();
    clean.pdf = crate::report_export::compile_pdf_from_data(&clean.data_json)
        .await
        .expect("the PDF compiles");
    let released = release(clean).expect("a clean compiled PDF clears the gate");
    assert!(hits(&released.bytes, "", "").is_empty());

    let mut pkg = package();
    let mut data: serde_json::Value = serde_json::from_str(&pkg.data_json).expect("data parses");
    data["executive_summary"]["narrative"] =
        serde_json::json!("Worth a conversation about the plan.");
    let planted = data.to_string();
    pkg.pdf = crate::report_export::compile_pdf_from_data(&planted)
        .await
        .expect("the PDF compiles");
    let failure = refused(release(pkg), "compiled PDF");
    let problems = internal_notes_problems(&failure);
    assert!(
        problems
            .iter()
            .any(|p| p.file.ends_with(".pdf") && p.location.starts_with("page ")),
        "{problems:?}"
    );
}
