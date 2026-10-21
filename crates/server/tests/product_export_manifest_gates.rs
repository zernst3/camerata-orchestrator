#![allow(clippy::unwrap_used)]
//! Gates 6 and 7 of `docs/design/2026-10-20_machine-readable-exports.md` §Testing, over the
//! REAL product ZIP assembly (`product_export_readme` + `build_product_zip`), the single
//! assembly point both `export_product` (HTTP) and `camerata inspect --export` route through.
//!
//! - **Gate 6, the corruption clause.** The coverage clause ("every `files[]` sha256 equals the
//!   actual hash; `files[]` covers every member but `manifest.json`") is pinned in
//!   `camerata_server`'s own unit tests. What it cannot show is that the check is capable of
//!   FAILING: a verifier that always passes satisfies it. These tests corrupt exactly one byte
//!   of each exported file in turn, re-zip it (valid CRCs, so the archive reader does not
//!   catch it for us) under the ORIGINAL manifest, and prove the sha256 check flags exactly
//!   that file, plus a control proving it flags nothing when nothing was touched.
//!
//! - **Gate 7, provenance parity.** The README is the human rendering of the manifest's
//!   `inspection` block. Both are rendered from ONE [`ManifestProvenance`], and these tests
//!   hold them equal field by field (commit, engine version, corpus version,
//!   rules-last-reviewed, repos, generated-at), including the absent-value case, where the
//!   README must omit the line rather than print a placeholder while the manifest carries an
//!   explicit `null`.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};

use camerata_server::onboard::{AuditedRef, Finding, LanguageVolume, ScanProvenance, ScanReport};
use camerata_server::report_export::{build_report_json, AuditReportJson, ReportOptions};
use camerata_server::{build_product_zip, product_export_readme, ManifestProvenance};
use sha2::{Digest, Sha256};

const STEM: &str = "acme-inspection";

fn report() -> ScanReport {
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
        repos: vec!["acme/portal".to_string(), "acme/web".to_string()],
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
        ledger: camerata_server::scan_ledger::ScanLedger::new(),
    }
}

fn report_json() -> AuditReportJson {
    build_report_json(
        &report().seal(),
        &HashMap::new(),
        None,
        &ReportOptions::default(),
    )
}

/// Assemble the ZIP exactly as the two real entry points do: README rendered from the
/// provenance, then the ZIP (and so the manifest) built from the SAME provenance value.
fn build_zip(provenance: &ManifestProvenance) -> Vec<u8> {
    let readme = product_export_readme(STEM, provenance);
    let extra = vec![
        ("csv/all-findings.csv".to_string(), b"a,b\n1,2\n".to_vec()),
        (
            "json/all-findings.json".to_string(),
            b"[{\"a\":1}]\n".to_vec(),
        ),
    ];
    build_product_zip(
        STEM,
        b"%PDF-1.7 fake pdf bytes for the corruption gate",
        b"PK fake xlsx bytes for the corruption gate",
        b"{\"findings\":[]}\n",
        &readme,
        &extra,
        provenance,
    )
    .expect("zip assembles")
}

/// A provenance with every optional field present, so no parity check can pass on `None == None`.
fn full_provenance() -> ManifestProvenance {
    let json = report_json();
    ManifestProvenance {
        commit: Some("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678".to_string()),
        corpus_version: Some("sha256:9f86d081884c7d659a2feaa0c55ad015".to_string()),
        rules_last_reviewed: Some("2026-10-18".to_string()),
        ..ManifestProvenance::from_cover(&json.cover)
    }
}

fn read_entries(zip_bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).expect("valid zip");
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).unwrap();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        out.insert(entry.name().to_string(), bytes);
    }
    out
}

/// Re-zip `entries` with fresh, VALID CRCs. Corrupting a byte inside the original deflate stream
/// would trip the archive reader's own CRC check and prove nothing about the manifest; a
/// tamper-then-recompress is the scenario the manifest hashes exist for.
fn rezip(entries: &BTreeMap<String, Vec<u8>>) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, bytes) in entries {
        writer.start_file(name.clone(), options).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The package verifier a recipient would run: every `files[]` entry's claimed sha256 against
/// the archive member's actual hash. Returns the paths that FAIL, covering all three ways it can:
/// a hash mismatch, a listed file that is absent, and a member the manifest does not list.
fn manifest_failures(zip_bytes: &[u8]) -> Vec<String> {
    let entries = read_entries(zip_bytes);
    let manifest: serde_json::Value =
        serde_json::from_slice(entries.get("manifest.json").expect("manifest.json present"))
            .expect("manifest.json parses");
    let mut failures = Vec::new();
    let mut listed = Vec::new();
    for file in manifest["files"].as_array().unwrap() {
        let path = file["path"].as_str().unwrap().to_string();
        let claimed = file["sha256"].as_str().unwrap();
        match entries.get(&path) {
            Some(bytes) if sha256_hex(bytes) == claimed => {}
            _ => failures.push(path.clone()),
        }
        listed.push(path);
    }
    for name in entries.keys() {
        if name != "manifest.json" && !listed.contains(name) {
            failures.push(name.clone());
        }
    }
    failures.sort();
    failures
}

// ── Gate 6 ────────────────────────────────────────────────────────────────────────────────

/// Control: an untouched package, re-zipped, verifies clean. Without this the corruption tests
/// below would also pass for a verifier that rejects everything.
#[test]
fn gate6_an_untouched_package_verifies_clean_after_a_rezip() {
    let zip_bytes = build_zip(&full_provenance());
    assert_eq!(manifest_failures(&zip_bytes), Vec::<String>::new());
    assert_eq!(
        manifest_failures(&rezip(&read_entries(&zip_bytes))),
        Vec::<String>::new(),
        "re-zipping must not itself disturb any hash"
    );
}

/// Flip ONE bit of ONE byte in each exported file in turn, under the original manifest. The
/// check must flag exactly that file: not nothing (a miss), and not the others (a verifier that
/// cannot localize is not a usable integrity check).
#[test]
fn gate6_one_corrupted_byte_in_any_exported_file_is_caught_and_localized() {
    let zip_bytes = build_zip(&full_provenance());
    let original = read_entries(&zip_bytes);
    let targets: Vec<&String> = original.keys().filter(|n| *n != "manifest.json").collect();
    assert!(
        targets.len() >= 6,
        "pdf + xlsx + findings.json + README + csv + json expected: {targets:?}"
    );
    for target in targets {
        let mut tampered = original.clone();
        let bytes = tampered.get_mut(target).unwrap();
        assert!(!bytes.is_empty(), "{target} is empty; nothing to corrupt");
        let at = bytes.len() / 2;
        bytes[at] ^= 0x01;
        assert_eq!(
            manifest_failures(&rezip(&tampered)),
            vec![target.clone()],
            "flipping one bit of byte {at} in {target} must be flagged, and only {target}"
        );
    }
}

/// The other half of the same guarantee: a one-byte change to the manifest's own claim for a
/// file is a mismatch too (the manifest cannot be quietly edited to agree with a swapped file).
#[test]
fn gate6_a_file_swapped_without_updating_the_manifest_is_caught() {
    let zip_bytes = build_zip(&full_provenance());
    let mut swapped = read_entries(&zip_bytes);
    swapped.insert("csv/all-findings.csv".to_string(), b"a,b\n9,9\n".to_vec());
    assert_eq!(
        manifest_failures(&rezip(&swapped)),
        vec!["csv/all-findings.csv".to_string()]
    );
}

// ── Gate 7 ────────────────────────────────────────────────────────────────────────────────

/// What the README prints for each provenance line, parsed back out of the text. Parsed by
/// label so this test fails if the README omits, renames or mangles a line, and does not depend
/// on how `ManifestProvenance` renders it.
#[derive(Debug, Default, PartialEq)]
struct ReadmeProvenance {
    repos: Option<String>,
    generated: Option<String>,
    commit: Option<String>,
    engine_version: Option<String>,
    corpus_version: Option<String>,
    rules_last_reviewed: Option<String>,
}

fn parse_readme_provenance(readme: &str) -> ReadmeProvenance {
    let mut p = ReadmeProvenance::default();
    for line in readme.lines() {
        if let Some(v) = line.strip_prefix("Repos inspected: ") {
            p.repos = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Generated: ") {
            p.generated = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Commit: ") {
            p.commit = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Engine version: ") {
            p.engine_version = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Corpus version: ") {
            p.corpus_version = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("Rules last reviewed on ") {
            p.rules_last_reviewed = Some(v.to_string());
        }
    }
    p
}

fn readme_and_manifest(zip_bytes: &[u8]) -> (String, serde_json::Value) {
    let entries = read_entries(zip_bytes);
    let readme = String::from_utf8(entries["README.txt"].clone()).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&entries["manifest.json"]).unwrap();
    (readme, manifest)
}

fn manifest_str(manifest: &serde_json::Value, key: &str) -> Option<String> {
    manifest["inspection"][key].as_str().map(str::to_string)
}

/// With every field present, each README line equals the manifest's value for the same field.
#[test]
fn gate7_readme_provenance_lines_equal_the_manifest_provenance() {
    let provenance = full_provenance();
    let (readme, manifest) = readme_and_manifest(&build_zip(&provenance));
    let parsed = parse_readme_provenance(&readme);

    let manifest_repos: Vec<String> = manifest["inspection"]["repos"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        manifest_repos,
        vec!["acme/portal".to_string(), "acme/web".to_string()]
    );
    assert_eq!(parsed.repos, Some(manifest_repos.join(", ")));
    assert_eq!(
        parsed.generated,
        manifest["generated_at"].as_str().map(str::to_string)
    );
    assert_eq!(parsed.commit, manifest_str(&manifest, "commit"));
    assert_eq!(
        parsed.engine_version,
        manifest_str(&manifest, "engine_version")
    );
    assert_eq!(
        parsed.corpus_version,
        manifest_str(&manifest, "corpus_version")
    );
    assert_eq!(
        parsed.rules_last_reviewed,
        manifest_str(&manifest, "rules_last_reviewed")
    );

    // And none of them is vacuous.
    assert_eq!(
        parsed.commit.as_deref(),
        Some("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678")
    );
    assert_eq!(
        parsed.corpus_version.as_deref(),
        Some("sha256:9f86d081884c7d659a2feaa0c55ad015")
    );
    assert_eq!(parsed.rules_last_reviewed.as_deref(), Some("2026-10-18"));
    assert!(parsed
        .engine_version
        .as_deref()
        .is_some_and(|v| v.starts_with("camerata ")));
}

/// The README must carry EVERY non-null value the manifest's `inspection` block carries, found
/// generically rather than by a hand-kept field list. A field added to the manifest but not to
/// the README (the drift a hand-maintained pair would eventually produce) fails here without
/// this test being edited.
#[test]
fn gate7_every_value_in_the_manifest_inspection_block_appears_in_the_readme() {
    let (readme, manifest) = readme_and_manifest(&build_zip(&full_provenance()));
    let inspection = manifest["inspection"].as_object().unwrap();
    assert!(inspection.len() >= 5, "{inspection:?}");
    for (key, value) in inspection {
        let rendered: Vec<String> = match value {
            serde_json::Value::Null => continue,
            serde_json::Value::String(s) => vec![s.clone()],
            serde_json::Value::Array(items) => items
                .iter()
                .map(|i| i.as_str().expect("repos are strings").to_string())
                .collect(),
            other => panic!("inspection.{key} has an unrendered shape: {other}"),
        };
        for text in rendered {
            assert!(
                readme.contains(&text),
                "manifest inspection.{key} = {text:?} is not in the README:\n{readme}"
            );
        }
    }
    assert!(
        readme.contains(manifest["generated_at"].as_str().unwrap()),
        "generated_at must appear in the README too:\n{readme}"
    );
}

/// Absent values: the manifest carries an explicit `null`, and the README omits the line. It
/// never prints a placeholder or a fabricated date (the design: "omits the line rather than
/// print a fabricated date").
#[test]
fn gate7_absent_provenance_is_null_in_the_manifest_and_omitted_from_the_readme() {
    let json = report_json();
    let provenance = ManifestProvenance::from_cover(&json.cover);
    assert!(provenance.commit.is_none());
    assert!(provenance.corpus_version.is_none());
    assert!(provenance.rules_last_reviewed.is_none());

    let (readme, manifest) = readme_and_manifest(&build_zip(&provenance));
    let parsed = parse_readme_provenance(&readme);

    for key in ["commit", "corpus_version", "rules_last_reviewed"] {
        assert!(
            manifest["inspection"]
                .get(key)
                .is_some_and(serde_json::Value::is_null),
            "inspection.{key} must be present and null: {manifest}"
        );
    }
    assert_eq!(parsed.commit, None, "{readme}");
    assert_eq!(parsed.corpus_version, None, "{readme}");
    assert_eq!(parsed.rules_last_reviewed, None, "{readme}");
    assert!(
        !readme.contains("Rules last reviewed"),
        "no fabricated review date: {readme}"
    );

    // The always-present lines still agree.
    assert_eq!(
        parsed.engine_version,
        manifest_str(&manifest, "engine_version")
    );
    assert!(parsed.repos.is_some() && parsed.generated.is_some());
}
