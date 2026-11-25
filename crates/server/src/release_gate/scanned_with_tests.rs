//! Issue #341 (V1 item 39, step B): the package states the build an inspection was SCANNED with
//! (saved with the scan) and the build that MADE the package (the running binary). Each package
//! here goes through the production assembly and the release gate, whose provenance check holds
//! the README and `manifest.json` equal. Synthetic data only.

use super::*;
use crate::onboard::ScanBuild;

const OTHER_COMMIT: &str = "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b";
const SCAN_CORPUS: &str = "sha256:the-library-the-scan-loaded";

/// A build that is not the one running these tests.
fn other_build(dirty: bool) -> ScanBuild {
    ScanBuild {
        version: "0.3.9".to_string(),
        commit: Some(OTHER_COMMIT.to_string()),
        dirty,
        corpus_version: Some(SCAN_CORPUS.to_string()),
    }
}

fn this_build() -> ScanBuild {
    ScanBuild {
        corpus_version: Some(SCAN_CORPUS.to_string()),
        ..ScanBuild::package_build()
    }
}

fn report_scanned_with(scanned_with: Option<ScanBuild>) -> SealedReport {
    let mut report = scan_report();
    report.provenance.scanned_with = scanned_with;
    report.seal()
}

fn package_of(sealed: &SealedReport, options: &ReportOptions) -> ProductPackage {
    let reviewed = crate::report_export::ReviewedReport::unreviewed(sealed, &HashMap::new());
    let json = build_report_json(&reviewed, None, options);
    let data = render_data_json(&json).expect("data.json renders");
    assemble_package(
        &reviewed,
        None,
        options,
        STEM,
        (json, data, STAND_IN_PDF.to_vec()),
    )
    .expect("the package assembles")
}

fn internal() -> ReportOptions {
    ReportOptions {
        internal: true,
        ..client_options()
    }
}

/// The released internal package's README and manifest (the gate passed, so they agree).
fn released_readme_and_manifest(scanned_with: Option<ScanBuild>) -> (String, Value) {
    let package = package_of(&report_scanned_with(scanned_with), &internal());
    let zip = release_internal(package).expect("the gate passes: the README equals the manifest");
    let entries = read_zip_entries(&zip.bytes).expect("zip");
    let manifest: Value = serde_json::from_slice(&entries["manifest.json"]).expect("manifest");
    let readme = String::from_utf8(entries["README.txt"].clone()).expect("utf8");
    assert!(
        verify_readme_provenance(&readme, &manifest).is_empty(),
        "manifest equals README"
    );
    (readme, manifest)
}

fn build_lines(readme: &str) -> Vec<&str> {
    readme
        .lines()
        .filter(|l| {
            l.starts_with("Scanned with")
                || l.starts_with("Package built with")
                || l.starts_with("Engine version")
        })
        .collect()
}

#[test]
fn one_line_when_the_inspection_was_scanned_with_the_build_that_made_the_package() {
    let (readme, manifest) = released_readme_and_manifest(Some(this_build()));
    let package = ScanBuild::package_build();
    assert_eq!(
        build_lines(&readme),
        vec![format!(
            "Scanned with and package built with: {}",
            package.readme_label()
        )],
        "{readme}"
    );
    if let Some(commit) = crate::ENGINE_COMMIT {
        assert!(
            readme.contains(&format!(", build {}", &commit[..12])),
            "{readme}"
        );
    }
    let inspection = &manifest["inspection"];
    assert_eq!(inspection["scanned_with"]["commit"], json!(package.commit));
    assert_eq!(inspection["scanned_with"]["corpus_version"], SCAN_CORPUS);
    assert_eq!(
        inspection["package_built_with"]["commit"],
        json!(package.commit)
    );
    assert_eq!(
        inspection["package_built_with"]["dirty"],
        json!(package.dirty)
    );
    // Kept for readers of packages made before the two builds were recorded.
    assert_eq!(
        inspection["engine_version"],
        crate::ManifestProvenance::default().engine_version()
    );
}

#[test]
fn two_lines_when_another_build_scanned_the_inspection() {
    let (readme, manifest) = released_readme_and_manifest(Some(other_build(false)));
    assert_eq!(
        build_lines(&readme),
        vec![
            "Scanned with: Camerata 0.3.9, build 1a2b3c4d5e6f".to_string(),
            format!(
                "Package built with: {}",
                ScanBuild::package_build().readme_label()
            ),
        ],
        "{readme}"
    );
    assert!(
        !readme.contains(OTHER_COMMIT),
        "twelve characters: {readme}"
    );
    // The corpus version is the scan's, as saved, not the exporting build's.
    assert!(
        readme.contains(&format!("Corpus version: {SCAN_CORPUS}\n")),
        "{readme}"
    );
    let inspection = &manifest["inspection"];
    assert_eq!(inspection["corpus_version"], SCAN_CORPUS);
    assert_eq!(inspection["scanned_with"]["commit"], OTHER_COMMIT);
    assert_eq!(inspection["scanned_with"]["version"], "0.3.9");
    assert_eq!(inspection["scanned_with"]["dirty"], false);
    assert_eq!(
        inspection["package_built_with"]["commit"],
        json!(crate::ENGINE_COMMIT)
    );

    // A README that names the scanning build wrongly disagrees with the manifest.
    let tampered = readme.replace("build 1a2b3c4d5e6f", "build 000000000000");
    assert!(verify_readme_provenance(&tampered, &manifest)
        .iter()
        .any(|p| p.location == "Scanned with"));
    // So does one that drops the package line.
    let dropped: String = readme
        .lines()
        .filter(|l| !l.starts_with("Package built with"))
        .map(|l| format!("{l}\n"))
        .collect();
    assert!(verify_readme_provenance(&dropped, &manifest)
        .iter()
        .any(|p| p.location == "Scanned with"));
}

#[test]
fn an_inspection_saved_before_the_build_was_recorded_says_not_recorded() {
    let (readme, manifest) = released_readme_and_manifest(None);
    assert_eq!(
        build_lines(&readme),
        vec![
            "Scanned with: not recorded".to_string(),
            format!(
                "Package built with: {}",
                ScanBuild::package_build().readme_label()
            ),
        ],
        "{readme}"
    );
    assert!(manifest["inspection"]["scanned_with"].is_null());
    assert!(manifest["inspection"]["package_built_with"].is_object());
}

#[test]
fn a_scanning_build_with_uncommitted_changes_says_so() {
    let (readme, _) = released_readme_and_manifest(Some(other_build(true)));
    assert!(
        readme.contains(
            "Scanned with: Camerata 0.3.9, build 1a2b3c4d5e6f (built with uncommitted changes)\n"
        ),
        "{readme}"
    );
}

/// The cover carries the scanning build; its `engine_commit` and `corpus_version` keep meaning the
/// exporting build. `json/inspection.json` carries both.
#[test]
fn the_cover_and_inspection_json_carry_the_scanning_build_beside_the_exporting_one() {
    let package = package_of(&report_scanned_with(Some(other_build(false))), &internal());
    let cover = &package.report_json.cover;
    assert_eq!(cover.scanned_with, Some(other_build(false)));
    assert_eq!(cover.engine_commit.as_deref(), crate::ENGINE_COMMIT);
    assert_eq!(
        cover.corpus_version, None,
        "no corpus loaded by this export"
    );
    assert!(cover
        .inspected_with
        .as_deref()
        .is_some_and(|s| s.contains("(reference 1a2b3c4d5e6f)")));
    let inspection: Value = serde_json::from_slice(&package.findings_json).unwrap();
    assert_eq!(
        inspection["provenance"]["scanned_with"]["commit"],
        OTHER_COMMIT
    );
    assert_eq!(
        inspection["provenance"]["engine_commit"],
        json!(crate::ENGINE_COMMIT)
    );
}

/// A client package's README still carries no provenance line: the build lines are internal.
#[test]
fn a_client_readme_carries_no_build_line() {
    let package = package_of(
        &report_scanned_with(Some(other_build(false))),
        &client_options(),
    );
    assert!(
        build_lines(&package.readme).is_empty(),
        "{}",
        package.readme
    );
    assert!(
        client_readme_problems(&format!("{}Scanned with: Camerata 0.3.9\n", package.readme))
            .iter()
            .any(|p| p.message.contains("Scanned with"))
    );
}

/// An export made by another build than the one that scanned changes no finding row: the rows of
/// `json/inspection.json` and every dataset are byte-identical to the export made on the scanning
/// build, and no model is called (a stand-in `claude` on PATH would leave a mark).
#[test]
#[serial_test::serial(process_env)]
fn an_export_on_another_build_leaves_the_finding_rows_byte_identical_and_calls_no_model() {
    let bin = tempfile::tempdir().unwrap();
    let mark = bin.path().join("called");
    {
        use std::os::unix::fs::PermissionsExt;
        let claude = bin.path().join("claude");
        std::fs::write(
            &claude,
            format!("#!/bin/sh\ntouch '{}'\nexit 1\n", mark.display()),
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        bin.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let _path = camerata_test_env::EnvGuard::set("PATH", &path);
    let _proxy = camerata_test_env::EnvGuard::set("HTTPS_PROXY", "http://127.0.0.1:9");

    let on_scanning_build = package_of(&report_scanned_with(Some(this_build())), &client_options());
    let on_another_build = package_of(
        &report_scanned_with(Some(other_build(false))),
        &client_options(),
    );
    assert_eq!(
        serde_json::to_vec(&on_scanning_build.findings_export.findings).unwrap(),
        serde_json::to_vec(&on_another_build.findings_export.findings).unwrap(),
        "the finding rows"
    );
    assert!(!on_scanning_build.findings_export.findings.is_empty());
    assert_eq!(on_scanning_build.datasets, on_another_build.datasets);
    let rows = |p: &ProductPackage| -> Vec<u8> {
        let v: Value = serde_json::from_slice(&p.findings_json).unwrap();
        serde_json::to_vec(&v["findings"]).unwrap()
    };
    assert_eq!(rows(&on_scanning_build), rows(&on_another_build));
    assert!(!mark.exists(), "no model was called");

    // And the export code names no model seam at all.
    for (name, src) in [
        ("release_gate.rs", include_str!("../release_gate.rs")),
        ("report_export.rs", include_str!("../report_export.rs")),
        ("xlsx_export.rs", include_str!("../xlsx_export.rs")),
        (
            "client_provenance.rs",
            include_str!("../client_provenance.rs"),
        ),
    ] {
        for seam in ["LlmPort", "crate::llm::", "build_llm("] {
            assert!(!src.contains(seam), "{name} names {seam}");
        }
    }
}
