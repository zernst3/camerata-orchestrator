#![allow(clippy::unwrap_used)]
//! Gates 6 and 7 of `docs/design/2026-10-20_machine-readable-exports.md` §Testing, over the
//! REAL product ZIP assembly (`product_export_readme` + `build_product_zip`), the single
//! assembly point both `export_product` (HTTP) and `camerata inspect --export` route through.
//!
//! The comparison logic is PRODUCTION code (`camerata_server::release_gate`: `verify_manifest`,
//! `verify_readme_provenance`), the same functions the export release gate runs over every
//! package; these tests call them, so the test and the runtime check cannot drift.
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
//!   hold them equal field by field (commit per repo, engine version, corpus version,
//!   rules-last-reviewed, repos, generated-at), including the absent-value case, where the
//!   README must omit the line rather than print a placeholder while the manifest carries an
//!   explicit `null`.

use std::collections::{BTreeMap, HashMap};
use std::io::{Read, Write};

use camerata_server::onboard::{AuditedRef, Finding, LanguageVolume, ScanProvenance, ScanReport};
use camerata_server::release_gate::{
    parse_readme_provenance, read_zip_entries, verify_manifest, verify_readme_provenance,
};
use camerata_server::report_export::{build_report_json, AuditReportJson, ReportOptions};
use camerata_server::{
    assemble_unreleased_zip as build_product_zip, product_export_readme, ManifestProvenance,
};

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
    let mut provenance = ManifestProvenance::from_cover(&json.cover);
    // Both repos carry a commit (the fixture report only recorded one, so this also covers the
    // README rendering a commit line per repo), one of them dirty.
    provenance.repos[0].commit = Some("a1b2c3d4e5f60718293a4b5c6d7e8f9012345678".to_string());
    provenance.repos[1].commit = Some("0f1e2d3c4b5a69788796a5b4c3d2e1f001122334".to_string());
    provenance.repos[1].dirty = true;
    provenance.corpus_version = Some("sha256:9f86d081884c7d659a2feaa0c55ad015".to_string());
    provenance.rules_last_reviewed = Some("2026-10-18".to_string());
    provenance
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

/// The package verifier a recipient would run (`release_gate::verify_manifest`): the paths that
/// FAIL, covering a hash mismatch, a listed file that is absent, and a member the manifest does
/// not list.
fn manifest_failures(zip_bytes: &[u8]) -> Vec<String> {
    let entries = read_zip_entries(zip_bytes).expect("valid zip");
    let mut failures: Vec<String> = verify_manifest(&entries)
        .into_iter()
        .map(|problem| problem.file)
        .collect();
    failures.sort();
    failures.dedup();
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

    let manifest_repos = manifest["inspection"]["repos"].as_array().unwrap();
    let manifest_names: Vec<String> = manifest_repos
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        manifest_names,
        vec!["acme/portal".to_string(), "acme/web".to_string()]
    );
    assert_eq!(parsed.repos, Some(manifest_names.join(", ")));
    assert_eq!(
        parsed.generated,
        manifest["generated_at"].as_str().map(str::to_string)
    );
    // Each repo's commit, and whether its tree was dirty, is the same in both renderings.
    let manifest_commits: BTreeMap<String, (String, bool)> = manifest_repos
        .iter()
        .map(|r| {
            (
                r["name"].as_str().unwrap().to_string(),
                (
                    r["commit"].as_str().unwrap().to_string(),
                    r["dirty"].as_bool().unwrap(),
                ),
            )
        })
        .collect();
    assert_eq!(parsed.commits, manifest_commits);
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
        parsed.commits.get("acme/portal"),
        Some(&(
            "a1b2c3d4e5f60718293a4b5c6d7e8f9012345678".to_string(),
            false
        ))
    );
    assert_eq!(
        parsed.commits.get("acme/web"),
        Some(&("0f1e2d3c4b5a69788796a5b4c3d2e1f001122334".to_string(), true))
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
/// this test being edited. The comparison is `release_gate::verify_readme_provenance`, the
/// function the release gate runs on every package.
#[test]
fn gate7_every_value_in_the_manifest_inspection_block_appears_in_the_readme() {
    let (readme, manifest) = readme_and_manifest(&build_zip(&full_provenance()));
    assert!(
        manifest["inspection"].as_object().unwrap().len() >= 4,
        "{manifest}"
    );
    assert_eq!(
        verify_readme_provenance(&readme, &manifest),
        Vec::new(),
        "README and manifest disagree:\n{readme}"
    );
    assert!(
        readme.contains(manifest["generated_at"].as_str().unwrap()),
        "generated_at must appear in the README too:\n{readme}"
    );

    // The check must be able to FAIL: a README that drops a value the manifest carries is
    // flagged, found generically.
    let without_commit: String = readme
        .lines()
        .filter(|l| !l.starts_with("Commit ("))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !verify_readme_provenance(&without_commit, &manifest).is_empty(),
        "a README missing the commit lines must not verify"
    );
}

/// Absent values: the manifest carries an explicit `null`, and the README omits the line. It
/// never prints a placeholder or a fabricated date (the design: "omits the line rather than
/// print a fabricated date").
#[test]
fn gate7_absent_provenance_is_null_in_the_manifest_and_omitted_from_the_readme() {
    // A scan that recorded no git identity (not a git checkout): no commit for any repo.
    let mut json = report_json();
    json.cover.audited_refs.clear();
    let provenance = ManifestProvenance::from_cover(&json.cover);
    assert_eq!(provenance.repos.len(), 2);
    assert!(provenance.repos.iter().all(|r| r.commit.is_none()));
    assert!(provenance.corpus_version.is_none());
    assert!(provenance.rules_last_reviewed.is_none());

    let (readme, manifest) = readme_and_manifest(&build_zip(&provenance));
    let parsed = parse_readme_provenance(&readme);

    for key in ["corpus_version", "rules_last_reviewed"] {
        assert!(
            manifest["inspection"]
                .get(key)
                .is_some_and(serde_json::Value::is_null),
            "inspection.{key} must be present and null: {manifest}"
        );
    }
    assert!(parsed.commits.is_empty(), "{readme}");
    // Every repo entry is still present, with an explicit null commit.
    for repo in manifest["inspection"]["repos"].as_array().unwrap() {
        assert!(repo["commit"].is_null(), "{repo}");
        assert_eq!(repo["dirty"], false, "{repo}");
    }
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
