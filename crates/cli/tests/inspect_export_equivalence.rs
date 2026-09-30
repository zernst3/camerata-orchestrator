//! Byte-for-byte export equivalence: `camerata inspect`'s export-assembly
//! (`inspect_cmd::build_product_export_bytes`) must produce the IDENTICAL product-export zip
//! CONTENT that `POST /api/projects/:id/product-export` (`export_product` in
//! `camerata_server::lib`) would produce for the SAME `ScanReport` — proving the CLI calls the
//! same code path rather than a parallel re-implementation (see `crates/cli/src/inspect_cmd.rs`'s
//! module doc comment).
//!
//! `server_path_export` below is NOT a shortcut that calls `build_product_export_bytes` a
//! second time — it independently re-states `export_product`'s own handler body (same
//! sequence of `report_export`/`xlsx_export`/zip-assembly calls, written out here so a future
//! edit to either side that silently diverges from the other fails this test).
//!
//! No live model call: the `ScanReport` here is a hand-built fixture (the same style as
//! `report_export.rs`'s own unit tests and `crates/server/tests/generate_sample_report.rs`),
//! not run through the real scanner — only the PDF/xlsx/zip SERIALIZERS are exercised, which
//! are pure/synchronous aside from the `compile_pdf` shell-out to `typst`.
//!
//! Zip entries are compared by CONTENT, not raw container bytes: `build_product_zip` stamps
//! each entry with the real wall-clock time (`zip_now()`), so two independent calls a few
//! milliseconds apart are byte-identical in practice but not guaranteed to be if the call
//! straddles a 2-second DOS-timestamp boundary. Comparing per-entry content sidesteps that one
//! known source of nondeterminism (the same category of issue
//! `crates/server/tests/generate_sample_report.rs` documents for its own zip step) without
//! weakening what "byte-for-byte" actually means for the product: the four files' bytes.

use std::collections::{BTreeMap, HashMap};

use camerata::inspect_cmd::build_product_export_bytes;
use camerata_server::onboard::{AuditedRef, Finding, ScanProvenance, ScanReport};
use camerata_server::report_export::{self, ReportOptions};
use camerata_server::xlsx_export;

fn typst_on_path() -> bool {
    std::process::Command::new("typst")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some()
}

/// A `Finding` built from only its non-defaulted required fields (`repo`/`path`/`line`/
/// `rule_id`/`severity`/`snippet`/`detail`); every other field resolves through `Finding`'s own
/// `#[serde(default)]`s via a JSON round-trip — the same trick
/// `crates/server/tests/generate_sample_report.rs` uses, since `Finding` derives `Deserialize`
/// but has ~20 fields, most defaulted.
fn finding(rule_id: &str, repo: &str, path: &str, line: usize, severity: &str) -> Finding {
    serde_json::from_value(serde_json::json!({
        "repo": repo,
        "path": path,
        "line": line,
        "rule_id": rule_id,
        "severity": severity,
        "snippet": "let secret = \"literal\";",
        "detail": "a fixture finding for the export-equivalence test",
    }))
    .expect("Finding must deserialize from its required fields plus serde defaults")
}

/// A hand-built `ScanReport` fixture — NOT run through the real scanner. `ScanReport` derives
/// only `Serialize` (no `Deserialize`), so it's built from `ScanReport::gated`'s full-field
/// constructor via struct mutation, the same pattern several HTTP handlers in
/// `crates/server/src/lib.rs` already use (`let mut r = ScanReport::gated(&repos); r.gated =
/// false; ...`).
fn fixture_report() -> ScanReport {
    let repos = vec!["fixture/inspect-equivalence".to_string()];
    let mut report = ScanReport::gated(&repos);
    report.gated = false;
    report.message = None;
    report.files_scanned = 5;
    report.findings = vec![
        finding(
            "SEC-NO-HARDCODED-SECRETS-1",
            &repos[0],
            "src/config.rs",
            3,
            "high",
        ),
        finding(
            "ARCH-NO-SECRETS-IN-URL-1",
            &repos[0],
            "src/client.rs",
            10,
            "medium",
        ),
    ];
    report.provenance = ScanProvenance {
        audited_refs: vec![AuditedRef {
            repo: repos[0].clone(),
            sha: Some("abc1234567890abc1234567890abc1234567890".to_string()),
            branch: Some("main".to_string()),
            dirty: false,
        }],
        audited_rule_ids: vec![
            "SEC-NO-HARDCODED-SECRETS-1".to_string(),
            "ARCH-NO-SECRETS-IN-URL-1".to_string(),
        ],
        camerata_version: env!("CARGO_PKG_VERSION").to_string(),
        started_at: "2026-01-01T00:00:00Z".to_string(),
        finished_at: "2026-01-01T00:05:00Z".to_string(),
        ..Default::default()
    };
    report
}

/// Independently re-states `export_product`'s HTTP handler body (`crates/server/src/lib.rs`):
/// the same `report_export`/`xlsx_export`/zip-assembly call sequence, over the SAME report +
/// `None` corpus + default options `build_product_export_bytes` uses. Deliberately NOT a call
/// to `build_product_export_bytes` itself — see this file's module doc comment.
async fn server_path_export(report: &ScanReport) -> (String, Vec<u8>) {
    let dispositions: HashMap<String, report_export::DispositionWire> = HashMap::new();
    let mut options = ReportOptions::default();
    options.apply_env_defaults();

    let json = report_export::build_report_json(report, &dispositions, None, &options);
    let pdf_bytes = report_export::compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed with typst on PATH");
    let xlsx_bytes = xlsx_export::build_workbook(report, &dispositions, None, &options)
        .expect("build_workbook must succeed");
    let findings_export = xlsx_export::build_findings_export(
        report,
        &dispositions,
        None,
        &json,
        &options.chosen_options,
    );
    let findings_json_bytes =
        serde_json::to_vec_pretty(&findings_export).expect("findings.json must serialize");

    let stem = camerata_server::report_filename_stem(report);
    let readme = camerata_server::product_export_readme(&stem, &json);
    let zip_bytes = camerata_server::build_product_zip(
        &stem,
        &pdf_bytes,
        &xlsx_bytes,
        &findings_json_bytes,
        &readme,
    )
    .expect("build_product_zip must succeed");

    (stem, zip_bytes)
}

/// Every zip entry's CONTENT, keyed by name — see this file's module doc comment for why
/// content (not raw container bytes) is the right equivalence unit here.
fn zip_entry_contents(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    use std::io::Read;
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("must be a valid zip");
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).expect("zip entry by index");
        let name = file.name().to_string();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("read zip entry content");
        out.insert(name, buf);
    }
    out
}

#[tokio::test]
async fn cli_export_assembly_matches_the_server_product_export_path_byte_for_byte() {
    if !typst_on_path() {
        eprintln!(
            "skipping cli_export_assembly_matches_the_server_product_export_path_byte_for_byte: \
             typst not on PATH"
        );
        return;
    }

    let report = fixture_report();

    let (cli_stem, cli_zip) = build_product_export_bytes(&report, None)
        .await
        .expect("the CLI's export assembly must succeed");
    let (server_stem, server_zip) = server_path_export(&report).await;

    assert_eq!(
        cli_stem, server_stem,
        "the CLI and server paths must derive the identical filename stem"
    );

    let cli_entries = zip_entry_contents(&cli_zip);
    let server_entries = zip_entry_contents(&server_zip);

    let cli_names: Vec<&String> = cli_entries.keys().collect();
    let server_names: Vec<&String> = server_entries.keys().collect();
    assert_eq!(
        cli_names, server_names,
        "the CLI and server exports must contain the exact same 4 entries"
    );

    for (name, cli_bytes) in &cli_entries {
        let server_bytes = server_entries
            .get(name)
            .unwrap_or_else(|| panic!("server export is missing entry {name:?}"));
        assert_eq!(
            cli_bytes, server_bytes,
            "entry {name:?} must be byte-for-byte identical between the CLI inspect export \
             and the server product-export path"
        );
    }
}
