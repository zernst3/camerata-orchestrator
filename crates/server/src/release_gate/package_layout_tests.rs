//! The package layout is fixed before it becomes the public interface: `manifest.json` and
//! `README.txt` at the top level, every JSON file under `json/`, `json/inspection.json` for the
//! whole inspection and `json/decisions.json` for the decisions. The package is the REAL one
//! the production assembly builds, released through the gate.

use camerata_api_types::package_paths::{
    DECISIONS_JSON, INSPECTION_JSON, MANIFEST_JSON, README_TXT,
};

use super::tests::{package, rezip};
use super::*;

#[test]
fn a_real_package_holds_the_inspection_and_the_decisions_under_json_and_none_at_the_top() {
    let released = release(package()).expect("a clean package clears the gate");
    let entries = read_zip_entries(&released.bytes).expect("zip");
    assert!(
        entries.contains_key("json/inspection.json"),
        "{:?}",
        entries.keys()
    );
    assert!(
        entries.contains_key("json/decisions.json"),
        "{:?}",
        entries.keys()
    );
    assert!(
        !entries.contains_key("findings.json"),
        "{:?}",
        entries.keys()
    );
    assert!(
        !entries.contains_key("decisions.json"),
        "{:?}",
        entries.keys()
    );
    assert!(entries.contains_key("manifest.json"));
    assert!(entries.contains_key("README.txt"));
    // The only files at the top level are the manifest, the README, the PDF and the workbook.
    for name in entries.keys().filter(|n| !n.contains('/')) {
        assert!(
            name == "manifest.json"
                || name == "README.txt"
                || name.ends_with(".pdf")
                || name.ends_with(".xlsx"),
            "unexpected top-level file {name}"
        );
    }
}

#[test]
fn the_manifest_lists_every_file_with_its_checksum_at_its_real_path() {
    use sha2::{Digest, Sha256};
    let released = release(package()).expect("clean");
    let entries = read_zip_entries(&released.bytes).expect("zip");
    let manifest: serde_json::Value =
        serde_json::from_slice(&entries[MANIFEST_JSON]).expect("manifest parses");
    let listed = manifest["files"].as_array().expect("files list");
    let paths: std::collections::BTreeSet<&str> = listed
        .iter()
        .map(|f| f["path"].as_str().expect("path"))
        .collect();
    let present: std::collections::BTreeSet<&str> = entries
        .keys()
        .map(String::as_str)
        .filter(|n| *n != MANIFEST_JSON)
        .collect();
    assert_eq!(
        paths, present,
        "the manifest lists exactly the files of the ZIP"
    );
    for file in listed {
        let path = file["path"].as_str().expect("path");
        let digest = Sha256::digest(&entries[path]);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(file["sha256"].as_str(), Some(hex.as_str()), "{path}");
    }
    for needed in [INSPECTION_JSON, DECISIONS_JSON] {
        assert!(paths.contains(needed), "{needed} not in the manifest");
    }
}

#[test]
fn the_readme_names_each_file_at_its_new_path() {
    let released = release(package()).expect("clean");
    let entries = read_zip_entries(&released.bytes).expect("zip");
    let readme = String::from_utf8(entries[README_TXT].clone()).expect("utf8");
    for expected in [
        "json/inspection.json\n  The whole inspection in one file.",
        "json/decisions.json\n  Every decision record",
        "csv/ and json/\n  One file for each table of the workbook",
        "hold the same rows as json/inspection.json, as one\n  flat table.",
        "manifest.json\n  The list of the files in this ZIP with a checksum for each. It stays at the top\n  level because it describes the whole ZIP.",
    ] {
        assert!(readme.contains(expected), "missing {expected:?}:\n{readme}");
    }
    assert!(!readme.contains("\nfindings.json\n"), "{readme}");
    assert!(!readme.contains("\ndecisions.json\n"), "{readme}");
}

#[test]
fn the_gate_accepts_the_new_layout_and_refuses_a_file_missing_from_the_manifest() {
    let released = release(package()).expect("clean");
    let entries = read_zip_entries(&released.bytes).expect("zip");
    let report = audit_zip(&rezip(&entries), None);
    assert!(
        report.problems().next().is_none(),
        "{:?}",
        report.problems().collect::<Vec<_>>()
    );

    // The inspection file is in the ZIP but the manifest does not list it.
    let mut edited = entries.clone();
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&edited[MANIFEST_JSON]).expect("manifest");
    manifest["files"]
        .as_array_mut()
        .expect("files")
        .retain(|f| f["path"] != INSPECTION_JSON);
    edited.insert(
        MANIFEST_JSON.to_string(),
        serde_json::to_vec_pretty(&manifest).expect("serialize"),
    );
    let report = audit_zip(&rezip(&edited), None);
    assert!(
        report.problems().any(|(_, p)| p.file == INSPECTION_JSON),
        "{:?}",
        report.problems().collect::<Vec<_>>()
    );
}
