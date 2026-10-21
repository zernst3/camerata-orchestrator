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
//! Zip entries are compared by CONTENT, not raw container bytes, and the `-findings.xlsx`
//! entry is recursed into (it's itself a zip) and compared at ITS entry level too — two
//! known, real sources of wall-clock nondeterminism are handled the same way
//! `crates/server/tests/generate_sample_report.rs` documents for its own zip step, rather
//! than weakening what "byte-for-byte" means for the product:
//!   1. `build_product_zip` stamps the OUTER zip's entries with the real time (`zip_now()`);
//!      two calls a few milliseconds apart usually land in the same 2-second DOS-timestamp
//!      bucket, but aren't guaranteed to under load — sidestepped by comparing decompressed
//!      content instead of the container bytes.
//!   2. `xlsx_export::build_workbook` writes a literal "generated at"
//!      `chrono::Utc::now().format("%Y-%m-%d %H:%M UTC")` cell into a worksheet (minute
//!      precision) — under CI/full-suite load the two calls here CAN straddle a minute
//!      boundary. `normalize_generated_at` blanks that one pattern out of any UTF-8 leaf
//!      entry before comparing, so this test asserts on everything the product actually
//!      controls and nothing it doesn't.
//!   3. `typst` stamps the compiled PDF's own Info dict with `/CreationDate` and `/ModDate`,
//!      and the trailer's `/ID` with a content-and-time-derived identifier. The compile
//!      invocation sets neither `SOURCE_DATE_EPOCH` nor `--creation-timestamp`, so the PDF
//!      embeds the real clock at second precision.
//!   4. `rust_xlsxwriter` writes `dcterms:created` / `dcterms:modified` into the workbook's
//!      `docProps/core.xml` at second precision, with no document properties set to pin them.
//!
//! Sources 3 and 4 were UNHANDLED until 2026-10-11, and this comment previously asserted the
//! opposite — that nothing but the two cases above varied call-to-call. Both fired: Pass 3's
//! three added worksheets widened the gap between the two generations enough to straddle a
//! second boundary roughly one run in four. Note what 1's own wording conceded all along:
//! "usually land in the same 2-second bucket, but aren't guaranteed to." A test that is
//! usually right is a test that teaches you to rerun until green.
//!
//! WORTH FIXING AT THE SOURCE, NOT DONE HERE: setting `SOURCE_DATE_EPOCH` on the `typst`
//! invocation and pinning the workbook's `DocProperties` creation datetime to the SCAN time
//! (already present in the report data) would make both artifacts byte-reproducible. That is
//! arguably more correct for a client deliverable too — the metadata would say when the audit
//! ran rather than when the bytes were serialized — and it would let the product prove two
//! exports are the same report. It is a change to client-facing metadata, so it is routed
//! rather than taken here.

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

    // Pass-1 W1 seal fix: `report_export::build_report_json`/`xlsx_export::build_workbook`/
    // `build_findings_export` now take `&SealedReport` — seal the fixture here (a cheap clone;
    // this hand-built test fixture has no row-adding passes of its own left to run).
    let sealed = report.clone().seal();

    let json = report_export::build_report_json(&sealed, &dispositions, None, &options);
    let pdf_bytes = report_export::compile_pdf(&json)
        .await
        .expect("compile_pdf must succeed with typst on PATH");
    let xlsx_bytes = xlsx_export::build_workbook(&sealed, &dispositions, None, &options)
        .expect("build_workbook must succeed");
    let findings_export = xlsx_export::build_findings_export(
        &sealed,
        &dispositions,
        None,
        &json,
        &options.chosen_options,
    );
    let findings_json_bytes =
        serde_json::to_vec_pretty(&findings_export).expect("findings.json must serialize");

    let mut extra_files: Vec<(String, Vec<u8>)> =
        xlsx_export::build_csv_exports(&sealed, &dispositions, None, &options)
            .expect("csv exports must build");
    extra_files.extend(
        xlsx_export::build_json_exports(&sealed, &dispositions, None, &options)
            .expect("json exports must build"),
    );
    let provenance = camerata_server::ManifestProvenance::from_cover(&json.cover);

    let stem = camerata_server::report_filename_stem(report);
    let readme = camerata_server::product_export_readme(&stem, &provenance);
    let zip_bytes = camerata_server::build_product_zip(
        &stem,
        &pdf_bytes,
        &xlsx_bytes,
        &findings_json_bytes,
        &readme,
        &extra_files,
        &provenance,
    )
    .expect("build_product_zip must succeed");

    (stem, zip_bytes)
}

/// The ONE known wall-clock literal `xlsx_export::build_workbook` writes into a worksheet
/// cell (see this file's module doc comment, point 2) — matched and blanked out before
/// comparing any UTF-8 leaf entry.
fn generated_at_pattern() -> &'static regex::Regex {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        regex::Regex::new(r"\d{4}-\d{2}-\d{2} \d{2}:\d{2} UTC")
            .expect("the generated-at pattern must be a valid regex")
    })
}

/// Wall-clock stamps that a GENERATED artifact writes into its own bytes, matched on RAW BYTES
/// rather than UTF-8 text because a compiled PDF is binary and still carries a creation date.
///
/// These are sources 3 and 4 in this file's module doc comment. Both were unhandled, and the
/// previous version of this function asserted the opposite — that binary entries could not vary
/// call-to-call, so a binary mismatch was necessarily real. That claim was false, and both holes
/// fired: a full-suite run reported the `.pdf` entry mismatching, and a rerun of the same
/// unchanged tree reported `-findings.xlsx::docProps/core.xml` instead. A test whose failing
/// entry changes between runs on identical code is measuring the clock, not the product.
fn volatile_byte_patterns() -> &'static Vec<regex::bytes::Regex> {
    static RES: std::sync::OnceLock<Vec<regex::bytes::Regex>> = std::sync::OnceLock::new();
    RES.get_or_init(|| {
        [
            // 3a. The PDF's ENTIRE XMP metadata packet, as one unit.
            //
            //     This is deliberately a whole-packet rule rather than a list of volatile
            //     fields, and that choice is the point. Chasing fields individually went
            //     `xmp:ModifyDate` -> `xmp:CreateDate` -> `xmpMM:InstanceID` -> `xmpMM:DocumentID`,
            //     each discovered only when it happened to straddle a second boundary on some
            //     run, each "fixed" by adding one more pattern. That is lexical whack-a-mole:
            //     the same method, and the same failure mode, as the credential keyword list
            //     and the evidence gate's six phrases that this codebase's own audit findings
            //     (B6, C3) condemn. The category is the right unit: an XMP packet is generated
            //     document-identity metadata (timestamps, instance and document ids, producer
            //     string) and contains no product content, so none of it is what this test
            //     asserts about.
            //
            //     The tradeoff, stated so it is a decision and not an oversight: a drift that
            //     appeared ONLY inside XMP would be invisible here. That is accepted because
            //     every value XMP carries about the report itself (title, producer) is also
            //     carried in the Info dict and the document body, which are still compared.
            r"(?s)<\?xpacket begin.*?<\?xpacket end[^>]*\?>",
            r"(?s)<x:xmpmeta.*?</x:xmpmeta>",
            // 3b. PDF date strings outside XMP, i.e. the Info dict: `(D:20261011002156Z)`,
            //     `(D:20261011002156-04'00')`. Keyed on the `D:` form itself rather than on
            //     `/CreationDate`, so an Info-dict key this test has not seen is still covered.
            r"D:\d{14}(?:[Zz]|[+-]\d{2}'?\d{2}'?)?",
            // 3c. The PDF trailer's file identifier, which writers derive from content AND time.
            //
            //     Both PDF string forms, because the spec permits either and typst uses the
            //     literal form: `/ID[(DxEpGydYvZsZCAMLKCl0sw==)(DxEpGydYvZsZCAMLKCl0sw==)]`.
            //     An earlier version of this pattern accepted only hex `<...>` strings and so
            //     never matched a single real export.
            r"/ID ?\[\s*(?:<[0-9A-Fa-f]*>|\([^)]*\))\s*(?:<[0-9A-Fa-f]*>|\([^)]*\))\s*\]",
            // 4. ISO-8601 instants, for `rust_xlsxwriter`'s `dcterms:created` and
            //    `dcterms:modified` in the workbook's `docProps/core.xml` — which is NOT an XMP
            //    packet, so 3a does not reach it. The offset is optional and may be `Z` or
            //    numeric; an earlier version required a literal `Z` and silently missed every
            //    local-time stamp.
            r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:[Zz]|[+-]\d{2}:?\d{2})?",
        ]
        .iter()
        .map(|p| regex::bytes::Regex::new(p).expect("a volatile-field pattern must be valid regex"))
        .collect()
    })
}

/// Normalize a leaf entry's bytes before comparison, neutralizing every known wall-clock stamp
/// and nothing else.
///
/// Byte-level patterns run FIRST so they reach binary entries too; the human-readable
/// "generated at" cell is then blanked from UTF-8 entries. Anything that survives this and still
/// differs is a REAL equivalence failure: the CLI export path has drifted from the server
/// product-export path, which is the only thing this test exists to catch.
fn normalize_leaf(bytes: &[u8]) -> Vec<u8> {
    let mut out = bytes.to_vec();
    for re in volatile_byte_patterns() {
        out = re.replace_all(&out, &b"<volatile>"[..]).into_owned();
    }
    match std::str::from_utf8(&out) {
        Ok(text) => generated_at_pattern()
            .replace_all(text, "<generated-at>")
            .into_owned()
            .into_bytes(),
        Err(_) => out,
    }
}

/// Every zip entry's CONTENT, keyed by name, RECURSING into any entry that is itself a zip
/// (the embedded `-findings.xlsx` workbook, keyed as `<outer-name>::<inner-name>`) and
/// normalizing every UTF-8 leaf via [`normalize_leaf`]. See this file's module doc comment for
/// why content (not raw container bytes) is the right equivalence unit, and why the xlsx needs
/// its OWN entries compared rather than being treated as one opaque blob.
fn zip_entry_contents(bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut out = BTreeMap::new();
    flatten_zip_entries(bytes, "", &mut out);
    out
}

fn flatten_zip_entries(bytes: &[u8], prefix: &str, out: &mut BTreeMap<String, Vec<u8>>) {
    use std::io::Read;
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("must be a valid zip");
    for i in 0..archive.len() {
        let mut file = archive.by_index(i).expect("zip entry by index");
        let name = file.name().to_string();
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("read zip entry content");
        let key = format!("{prefix}{name}");
        if name.ends_with(".xlsx") {
            flatten_zip_entries(&buf, &format!("{key}::"), out);
        } else {
            out.insert(key, normalize_leaf(&buf));
        }
    }
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

    let (cli_stem, cli_zip) = build_product_export_bytes(&report.clone().seal(), None)
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
        assert_entry_bytes_eq(name, cli_bytes, server_bytes);
    }
}

/// Proof that [`normalize_leaf`] neutralizes wall-clock noise WITHOUT neutralizing real content.
///
/// This exists because the normalization grew four rules deep, one of which blanks an entire XMP
/// packet. Each rule widens what the equivalence test is willing to ignore, and a normalization
/// wide enough to pass anything would turn the whole test green and meaningless -- the same
/// failure class as a guard test that cannot fail, and as the E2 ceiling that was fully tested
/// and completely inert. So: every volatile form must normalize away, and a one-character
/// difference in product content must still survive.
#[test]
fn normalization_neutralizes_clock_noise_and_nothing_else() {
    // Two "exports" generated a second apart: every volatile form differs, content is identical.
    let a = b"<x:xmpmeta><xmp:CreateDate>2026-10-11T08:49:30-04:00</xmp:CreateDate>\
               <xmpMM:InstanceID>AAAA==</xmpMM:InstanceID></x:xmpmeta>\
               /Info(D:20261011084930-04'00')/ID[(AAAA==)(AAAA==)]\
               <dcterms:created>2026-10-11T08:49:30Z</dcterms:created>\
               Critical: hardcoded service-role key. generated at 2026-10-11 08:49 UTC";
    let b = b"<x:xmpmeta><xmp:CreateDate>2026-10-11T08:49:31-04:00</xmp:CreateDate>\
               <xmpMM:InstanceID>ZZZZ==</xmpMM:InstanceID></x:xmpmeta>\
               /Info(D:20261011084931-04'00')/ID[(ZZZZ==)(ZZZZ==)]\
               <dcterms:created>2026-10-11T08:49:31Z</dcterms:created>\
               Critical: hardcoded service-role key. generated at 2026-10-11 08:49 UTC";
    assert_ne!(
        a.to_vec(),
        b.to_vec(),
        "the fixture must actually differ before normalization, or this test proves nothing"
    );
    assert_eq!(
        normalize_leaf(a),
        normalize_leaf(b),
        "every volatile form (XMP packet, PDF date, trailer /ID, ISO-8601 instant) must normalize \
         away -- if this fails, the equivalence test will flake again"
    );

    // Same volatile values, ONE character of real content changed: must still differ.
    let c = b"<x:xmpmeta><xmp:CreateDate>2026-10-11T08:49:30-04:00</xmp:CreateDate></x:xmpmeta>\
               High: hardcoded service-role key.";
    let d = b"<x:xmpmeta><xmp:CreateDate>2026-10-11T08:49:30-04:00</xmp:CreateDate></x:xmpmeta>\
               High: hardcoded service-role keys.";
    assert_ne!(
        normalize_leaf(c),
        normalize_leaf(d),
        "normalization must NOT swallow a real content difference -- an equivalence test that \
         cannot fail is worse than no equivalence test, because it manufactures confidence"
    );
}

/// Compare one entry's bytes, and on mismatch report the FIRST differing offset with a readable
/// window around it.
///
/// A bare `assert_eq!` on two multi-megabyte `Vec<u8>` prints thousands of integers from the
/// start of each — identical prefixes, telling you nothing about where or why they diverge. That
/// is how sources 3 and 4 in the module doc comment stayed unhandled while the test flaked: the
/// failure was unreadable, so it read as noise. The window below is what makes an unhandled
/// wall-clock stamp self-identifying.
fn assert_entry_bytes_eq(name: &str, left: &[u8], right: &[u8]) {
    if left == right {
        return;
    }
    let first_diff = left
        .iter()
        .zip(right)
        .position(|(a, b)| a != b)
        .unwrap_or_else(|| left.len().min(right.len()));
    let start = first_diff.saturating_sub(60);
    let window = |b: &[u8]| {
        let end = (first_diff + 60).min(b.len());
        String::from_utf8_lossy(&b[start.min(b.len())..end]).replace(['\n', '\r'], "·")
    };
    panic!(
        "entry {name:?} must be byte-for-byte identical between the CLI inspect export and the \
         server product-export path.\n\
         lengths: cli={} server={}\n\
         first difference at byte {first_diff}\n\
         cli    …{}…\n\
         server …{}…\n\
         If the window shows a date or a time, it is an unhandled wall-clock stamp and belongs \
         in `volatile_byte_patterns`, NOT a real equivalence failure. Anything else is real \
         drift between the two export paths.",
        left.len(),
        right.len(),
        window(left),
        window(right),
    );
}
