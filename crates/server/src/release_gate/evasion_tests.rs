//! V1 item 34, gate evasions: the secrets check must see what a client sees. A key hidden by a
//! soft hyphen, split by a space, escaped in an unparseable `.json`, written as numeric XML
//! character references, kept in a workbook comment or document property, or printed only in
//! the PDF's rendered text is refused, naming the file. Every value is built at runtime by
//! `camerata_secrets::fixtures`.

use std::collections::BTreeMap;
use std::io::Write;

use camerata_secrets::fixtures::planted;
use camerata_secrets::SecretShape;

use super::tests::{package, plant, rewrite_xlsx_part, Surface};
use super::*;

fn blocked_secret_files(package: ProductPackage) -> Vec<String> {
    match release(package) {
        Err(ReleaseError::Blocked(failure)) => failure
            .report
            .checks
            .iter()
            .filter(|o| o.check == CheckId::Secrets)
            .flat_map(|o| o.problems.iter().map(|p| p.file.clone()))
            .collect(),
        Err(other) => panic!("expected the gate to block, got {other}"),
        Ok(_) => panic!("expected the gate to block, but the package was released"),
    }
}

fn key() -> String {
    planted(SecretShape::StripeLiveSecretKey).value
}

#[test]
fn a_soft_hyphen_inside_a_key_does_not_hide_it_from_the_gate() {
    let value = key();
    let (head, tail) = value.split_at(14);
    let hidden = format!("{head}\u{00AD}{tail}");
    for surface in [Surface::DataJson, Surface::Readme, Surface::DatasetCsv] {
        let mut pkg = package();
        plant(&mut pkg, surface, &hidden);
        assert!(!blocked_secret_files(pkg).is_empty(), "{surface:?}");
    }
}

#[test]
fn a_key_split_by_a_space_is_refused_by_the_gate() {
    let value = key();
    let (head, tail) = value.split_at(16);
    for surface in [Surface::DataJson, Surface::Readme, Surface::DatasetCsv] {
        let mut pkg = package();
        plant(&mut pkg, surface, &format!("{head} {tail}"));
        assert!(!blocked_secret_files(pkg).is_empty(), "{surface:?}");
    }
}

#[test]
fn prose_with_a_vendor_word_and_spaces_is_not_a_split_key() {
    let mut pkg = package();
    plant(
        &mut pkg,
        Surface::Readme,
        "the sk key and the AKIA prefix are named in this sentence, nothing more",
    );
    assert!(release(pkg).is_ok());
}

#[test]
fn an_unparseable_json_member_is_scanned_with_its_escapes_decoded() {
    let value = key();
    // `s` is `s`: any reader that opens the file sees the key.
    let escaped = value.replacen('s', "\\u0073", 1);
    assert_ne!(escaped, value);
    let broken = format!("{{\"note\": \"{escaped}\", \"truncated\": ");
    let problems = secrets::json_bytes_problems("json/odd.json", broken.as_bytes());
    assert!(!problems.is_empty(), "the escaped key was not found");
    // Every other escape form decodes too: a surrogate pair, `\/`, `\n`.
    let tricky = format!("{{\"a\": \"\\ud83d\\ude00\\/\\n{escaped}");
    assert!(!secrets::json_bytes_problems("json/odd.json", tricky.as_bytes()).is_empty());
    // A broken escape is left as written and does not panic.
    assert!(secrets::json_bytes_problems("json/odd.json", b"{\"a\": \"\\u12\\uzz\\").is_empty());
}

#[test]
fn numeric_xml_character_references_in_a_workbook_do_not_hide_a_key() {
    let value = key();
    let decimal: String = value.chars().map(|c| format!("&#{};", c as u32)).collect();
    let hex: String = value
        .chars()
        .map(|c| format!("&#x{:X};", c as u32))
        .collect();
    for reference in [decimal, hex] {
        let mut pkg = package();
        pkg.xlsx = rewrite_xlsx_part(&pkg.xlsx, "xl/sharedStrings.xml", |xml| {
            xml.replacen("</sst>", &format!("<si><t>{reference}</t></si></sst>"), 1)
        });
        let files = blocked_secret_files(pkg);
        assert!(files.iter().any(|f| f.ends_with(".xlsx")), "{files:?}");
    }
}

#[test]
fn a_key_in_a_workbook_property_or_comment_is_refused() {
    let value = key();
    let mut pkg = package();
    pkg.xlsx = rewrite_xlsx_part(&pkg.xlsx, "docProps/core.xml", |xml| {
        xml.replacen(
            "</cp:coreProperties>",
            &format!("<dc:description>{value}</dc:description></cp:coreProperties>"),
            1,
        )
    });
    let files = blocked_secret_files(pkg);
    assert!(files.iter().any(|f| f.ends_with(".xlsx")), "{files:?}");

    // A comment part: two runs, the key split between them.
    let (head, tail) = value.split_at(12);
    let mut entries = read_zip_entries(&package().xlsx).expect("a zip");
    entries.insert(
        "xl/comments1.xml".to_string(),
        format!(
            "<comments><commentList><comment ref=\"A1\"><text><r><t>{head}</t></r><r><t>{tail}</t></r></text></comment></commentList></comments>"
        )
        .into_bytes(),
    );
    let with_comment = zip_of(&entries);
    assert!(!secrets::xlsx_problems("report.xlsx", &with_comment).is_empty());
    // The clean workbook has none.
    assert!(secrets::xlsx_problems("report.xlsx", &package().xlsx).is_empty());
}

fn zip_of(entries: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, bytes) in entries {
        writer.start_file(name.clone(), options).expect("start");
        writer.write_all(bytes).expect("write");
    }
    writer.finish().expect("finish").into_inner()
}

#[test]
fn a_key_only_in_the_pdfs_rendered_text_is_refused() {
    let value = key();
    let mut entries = BTreeMap::new();
    entries.insert("report.pdf".to_string(), b"%PDF stand-in".to_vec());
    let mut pages = BTreeMap::new();
    pages.insert(
        "report.pdf".to_string(),
        text_style::PdfText::Pages(vec!["page one".to_string(), format!("key: {value}")]),
    );
    let problems = archive_secret_problems(&entries, None, &pages);
    assert!(
        problems.iter().any(|p| p.file == "report.pdf"),
        "{problems:?}"
    );
    // A PDF whose text cannot be read is not a PDF that passed.
    pages.insert(
        "report.pdf".to_string(),
        text_style::PdfText::Unreadable("broken".to_string()),
    );
    assert!(!archive_secret_problems(&entries, None, &pages).is_empty());
    // A stand-in the check does not judge adds nothing.
    pages.insert("report.pdf".to_string(), text_style::PdfText::NotJudged);
    assert!(archive_secret_problems(&entries, None, &pages).is_empty());
}
