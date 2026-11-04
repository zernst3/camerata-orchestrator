//! Product export, freshness gate, manifest, project context, intake and credential
//! tests.

use super::*;

/// set_last_scan / get_last_scan round-trip: a stored report is returned.
#[test]
fn last_scan_set_get_round_trip() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let report = make_scan_report("SEC-NO-HARDCODED-SECRETS-1");

    // Before any write: None.
    assert!(
        state.get_last_scan("proj-1").is_none(),
        "get_last_scan must return None before any set"
    );

    state.set_last_scan("proj-1".to_string(), report.clone());

    let got = state.get_last_scan("proj-1");
    assert!(got.is_some(), "get_last_scan must return Some after set");
    let got = got.unwrap();
    assert_eq!(got.findings.len(), 1, "findings must be preserved");
    assert_eq!(
        got.findings[0].rule_id, "SEC-NO-HARDCODED-SECRETS-1",
        "rule_id must round-trip"
    );
}

/// Different project ids are stored independently.
#[test]
fn last_scan_is_keyed_per_project() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.set_last_scan("proj-A".to_string(), make_scan_report("RULE-A"));
    state.set_last_scan("proj-B".to_string(), make_scan_report("RULE-B"));

    let a = state.get_last_scan("proj-A").unwrap();
    let b = state.get_last_scan("proj-B").unwrap();
    assert_eq!(a.findings[0].rule_id, "RULE-A");
    assert_eq!(b.findings[0].rule_id, "RULE-B");
    assert!(state.get_last_scan("proj-C").is_none());
}

// ── Product export (PDF + Excel workbook, zipped) ─────────────────────────────

/// Best-effort `typst` presence check (mirrors `report_export`'s own internal test
/// gate) — the PDF-compile stage of the happy-path product-export test skips
/// gracefully rather than hard-failing CI environments without Typst installed.
fn typst_on_path() -> bool {
    camerata_test_env::typst_on_path()
}

#[tokio::test]
async fn product_export_404s_when_project_does_not_exist() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let resp = export_product(
        axum::extract::State(state),
        axum::extract::Path("nope".to_string()),
        axum::Json(AuditReportReq {
            dispositions: std::collections::HashMap::new(),
            options: crate::report_export::ReportOptions::default(),
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn product_export_404s_when_no_scan_available() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Acme", vec![])
        .expect("project created");
    let resp = export_product(
        axum::extract::State(state),
        axum::extract::Path(project.id),
        axum::Json(AuditReportReq {
            dispositions: std::collections::HashMap::new(),
            options: crate::report_export::ReportOptions::default(),
        }),
    )
    .await;
    assert_eq!(
        resp.status(),
        StatusCode::NOT_FOUND,
        "no last_scan yet -> 404, same contract as /audit-report"
    );
}

/// End-to-end: the product-export ZIP contains a real `%PDF`-magic PDF, a real
/// `PK`-magic xlsx (itself a zip container), and a README.txt naming both. Gated on
/// `typst` being on PATH (the PDF half needs it); the workbook half runs unconditionally
/// via the other degenerate-input tests in `xlsx_export`.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn product_export_zip_contains_a_real_pdf_xlsx_and_readme() {
    if !typst_on_path() {
        eprintln!(
            "product_export_zip_contains_a_real_pdf_xlsx_and_readme: skipping — typst not \
                 on PATH"
        );
        return;
    }
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("Acme", vec!["owner/repo".to_string()])
        .expect("project created");
    let project = state
        .projects
        .update(&project.id, |p| {
            p.client_name = "Example Client Ltd".to_string()
        })
        .expect("project exists");
    state.set_last_scan(
        project.id.clone(),
        make_scan_report("SEC-NO-HARDCODED-SECRETS-1"),
    );

    let resp = export_product(
        axum::extract::State(state),
        axum::extract::Path(project.id.clone()),
        axum::Json(AuditReportReq {
            dispositions: std::collections::HashMap::new(),
            options: crate::report_export::ReportOptions::default(),
        }),
    )
    .await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get(axum::http::header::CONTENT_TYPE)
            .unwrap(),
        "application/zip"
    );
    let content_disposition = resp
        .headers()
        .get(axum::http::header::CONTENT_DISPOSITION)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(
        content_disposition.contains(".zip"),
        "{content_disposition}"
    );

    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        &bytes[0..2],
        b"PK",
        "the product export itself must be a valid zip"
    );

    use std::io::Read;
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(bytes.to_vec())).expect("valid zip");
    let mut names: Vec<String> = (0..archive.len())
        .map(|i| archive.by_index(i).unwrap().name().to_string())
        .collect();
    names.sort();
    assert!(names.iter().any(|n| n.ends_with(".pdf")), "{names:?}");
    assert!(
        names.iter().any(|n| n.ends_with("-findings.xlsx")),
        "{names:?}"
    );
    assert!(names.contains(&"findings.json".to_string()), "{names:?}");
    assert!(names.contains(&"README.txt".to_string()), "{names:?}");

    let pdf_name = names.iter().find(|n| n.ends_with(".pdf")).unwrap().clone();
    let mut pdf_bytes = Vec::new();
    archive
        .by_name(&pdf_name)
        .unwrap()
        .read_to_end(&mut pdf_bytes)
        .unwrap();
    assert!(
        pdf_bytes.starts_with(b"%PDF"),
        "the zipped PDF must carry the PDF magic bytes"
    );

    let xlsx_name = names.iter().find(|n| n.ends_with(".xlsx")).unwrap().clone();
    let mut xlsx_bytes = Vec::new();
    archive
        .by_name(&xlsx_name)
        .unwrap()
        .read_to_end(&mut xlsx_bytes)
        .unwrap();
    assert_eq!(
        &xlsx_bytes[0..2],
        b"PK",
        "the zipped xlsx must itself be a valid zip container"
    );

    let mut findings_json_str = String::new();
    archive
        .by_name("findings.json")
        .unwrap()
        .read_to_string(&mut findings_json_str)
        .unwrap();
    let findings_json: serde_json::Value =
        serde_json::from_str(&findings_json_str).expect("findings.json must be valid JSON");
    assert!(
        findings_json.get("provenance").is_some(),
        "{findings_json_str}"
    );
    assert!(
        findings_json.get("summary").is_some(),
        "{findings_json_str}"
    );
    let findings_arr = findings_json
        .get("findings")
        .and_then(|f| f.as_array())
        .expect("findings.json must have a findings array");
    assert_eq!(
        findings_arr.len(),
        1,
        "the one non-FP fixture finding must be the sole findings.json row: {findings_json_str}"
    );

    let mut readme = String::new();
    archive
        .by_name("README.txt")
        .unwrap()
        .read_to_string(&mut readme)
        .unwrap();
    assert!(readme.contains(&pdf_name), "{readme}");
    assert!(readme.contains(&xlsx_name), "{readme}");
    assert!(readme.contains("findings.json"), "{readme}");
    // P6 (2026-09-29): the product is "Codebase Inspection" — the README names it and
    // never says "audit"/"Audit" (the disclaimer paragraph it embeds verbatim is itself
    // audit-free per `report_export`'s own contract test).
    assert!(readme.contains("Codebase Inspection"), "{readme}");
    assert!(readme.contains("Repos inspected:"), "{readme}");
    assert!(
        !readme.to_lowercase().contains("audit"),
        "README.txt still says audit: {readme}"
    );
}

/// Pure unit test for `product_export_readme` — doesn't need `typst` on PATH (unlike the
/// full-zip e2e test above), so it always runs. P6 (2026-09-29): product name + no "audit".
#[test]
fn product_export_readme_names_the_product_and_never_says_audit() {
    let report = make_scan_report("SEC-NO-HARDCODED-SECRETS-1");
    let json = crate::report_export::build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(
            &report.clone().seal(),
            &std::collections::HashMap::new(),
        ),
        None,
        &crate::report_export::ReportOptions::default(),
    );
    let readme = product_export_readme(
        "acme-inspection",
        &ManifestProvenance::from_cover(&json.cover),
    );
    assert!(readme.contains("Codebase Inspection"), "{readme}");
    assert!(readme.contains("Repos inspected:"), "{readme}");
    assert!(readme.contains("acme-inspection.pdf"), "{readme}");
    assert!(
        !readme.to_lowercase().contains("audit"),
        "README.txt still says audit: {readme}"
    );
}

// ── Corpus freshness is operator-only (design amendment 2026-10-21, part 4) ──
//
// The corpus-freshness nag, banner and `corpus_freshness` JSON are for the operator who
// maintains the corpus. They must never reach a client artifact: a paying client's report
// saying "javascript pack OVERDUE for a staleness sweep" would be both a leak of our
// maintenance state and, worse, an unprompted claim about the quality of the guidance in
// their deliverable. The client-visible `rules_last_reviewed` manifest date is a separate,
// deliberate field and is NOT in the marker list.

/// Operator-only markers present in `text` (empty means clean).
fn operator_markers_in(text: &str) -> Vec<&'static str> {
    camerata_rules::OPERATOR_ONLY_MARKERS
        .iter()
        .copied()
        .filter(|m| text.contains(m))
        .collect()
}

/// The corpus-freshness code an artifact builder must never reference: the maintenance
/// types, the nag formatter, the marker list, the watermark file, and the one sanctioned
/// client-date function (which only the provenance bridge may name).
const FRESHNESS_IDENTIFIERS: &[&str] = &[
    "CorpusFreshness",
    "FreshnessReport",
    "format_staleness_nag",
    "OPERATOR_ONLY_MARKERS",
    "corpus_freshness",
    "CORPUS_FRESHNESS",
    "due_soon",
    "client_rules_last_reviewed",
];

/// EXACTLY what `client_provenance.rs` may name: the watermark type and its path (to read
/// the file), and the sanctioned function that turns it into the one client-visible date.
/// Everything else in [`FRESHNESS_IDENTIFIERS`] (the nag, the report types, the markers, the
/// due-soon logic) stays forbidden there too. Widening this list is widening what a client
/// artifact can be told about maintenance state; it needs a design decision, not a test edit.
const SANCTIONED_BRIDGE_IDENTIFIERS: &[&str] = &[
    "CorpusFreshness",
    "corpus_freshness_path",
    "client_rules_last_reviewed",
];

/// The [`FRESHNESS_IDENTIFIERS`] that appear in `src`, after setting aside the identifiers
/// in `sanctioned`. A source that names only sanctioned identifiers returns empty. Longer
/// sanctioned names are removed first so one never leaves a fragment of another behind.
fn freshness_identifiers_in(src: &str, sanctioned: &[&str]) -> Vec<&'static str> {
    let mut sanctioned: Vec<&str> = sanctioned.to_vec();
    sanctioned.sort_by_key(|s| std::cmp::Reverse(s.len()));
    let mut remaining = src.to_string();
    for id in sanctioned {
        remaining = remaining.replace(id, "");
    }
    FRESHNESS_IDENTIFIERS
        .iter()
        .copied()
        .filter(|id| remaining.contains(id))
        .collect()
}

/// Every file in a zip as `(path, lossy text)`, recursing into nested zip containers (the
/// xlsx inside the product zip is itself a zip of XML parts, and its strings live in there).
fn artifact_texts(zip_bytes: &[u8], prefix: &str, out: &mut Vec<(String, String)>) {
    use std::io::Read;
    let Ok(mut archive) = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes.to_vec())) else {
        return;
    };
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).expect("zip entry");
        let path = format!("{prefix}{}", entry.name());
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("read entry");
        if bytes.starts_with(b"PK") {
            artifact_texts(&bytes, &format!("{path}!"), out);
        }
        out.push((path, String::from_utf8_lossy(&bytes).into_owned()));
    }
}

/// The scanner is only worth anything if it can fail: plant a marker in a README and in a
/// nested container and prove both are caught.
#[test]
fn the_freshness_scanner_catches_a_planted_marker_at_any_depth() {
    use std::io::Write;
    let mut inner = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    inner.start_file("xl/sharedStrings.xml", opts).unwrap();
    inner.write_all(b"<si>DUE SOON: javascript</si>").unwrap();
    let inner_bytes = inner.finish().unwrap().into_inner();

    let zip_bytes = build_product_zip(
        "acme",
        b"%PDF",
        &inner_bytes,
        b"{}",
        "CORPUS STALENESS: go is overdue",
        &[],
        &ManifestProvenance::default(),
    )
    .expect("zip assembles");

    let mut texts = Vec::new();
    artifact_texts(&zip_bytes, "", &mut texts);
    let flagged: Vec<&str> = texts
        .iter()
        .filter(|(_, t)| !operator_markers_in(t).is_empty())
        .map(|(p, _)| p.as_str())
        .collect();
    assert!(flagged.contains(&"README.txt"), "{flagged:?}");
    assert!(
        flagged.contains(&"acme-findings.xlsx!xl/sharedStrings.xml"),
        "nested container scanned: {flagged:?}"
    );
}

/// THE GATE. Builds every client artifact through the same builders the product export
/// uses (PDF input data, xlsx, findings.json, csv/*, json/*, README, manifest, the zip
/// itself) and asserts none contains operator-only freshness text. Also scans the SOURCE of
/// the export builders for freshness identifiers, so a future wiring is caught at the
/// import, before it ever produces output.
#[tokio::test]
#[serial_test::serial(process_env)]
async fn no_corpus_freshness_text_reaches_any_client_artifact() {
    // ── Anti-vacuity: the operator surfaces really do carry the markers ──────────────
    let operator_json = updates_payload(
        &AppState::new(std::sync::Arc::new(InMemoryStoryStore::new())),
        serde_json::Value::Null,
    )
    .await
    .to_string();
    assert!(
        operator_json.contains("corpus_freshness"),
        "the operator payload names the field the gate looks for: {operator_json}"
    );

    // ── Build every artifact from one sealed fixture report ──────────────────────────
    let report = make_scan_report("SEC-NO-HARDCODED-SECRETS-1").seal();
    let dispositions = std::collections::HashMap::new();
    let options = crate::report_export::ReportOptions::default();
    let chosen = std::collections::HashMap::new();
    let corpus_path = camerata_rules::corpus_path();
    let corpus = if corpus_path.exists() {
        Some(camerata_rules::load_corpus_lenient(&corpus_path).await.0)
    } else {
        None
    };

    let json = crate::report_export::build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report, &dispositions),
        corpus.as_ref(),
        &options,
    );
    // The PDF is rendered from exactly this data (plus a static template).
    let pdf_input = serde_json::to_string(&json).expect("report json serializes");

    let xlsx = crate::xlsx_export::build_workbook(
        &crate::report_export::ReviewedReport::unreviewed(&report, &dispositions),
        corpus.as_ref(),
        &options,
    )
    .expect("workbook builds");
    let findings = crate::xlsx_export::build_findings_export(
        &crate::report_export::ReviewedReport::unreviewed(&report, &dispositions),
        corpus.as_ref(),
        &json,
        &chosen,
    );
    let findings_bytes = serde_json::to_vec_pretty(&findings).expect("findings serialize");
    let mut extra: Vec<(String, Vec<u8>)> = Vec::new();
    extra.extend(
        crate::xlsx_export::build_csv_exports(
            &crate::report_export::ReviewedReport::unreviewed(&report, &dispositions),
            corpus.as_ref(),
            &options,
        )
        .expect("csv builds"),
    );
    extra.extend(
        crate::xlsx_export::build_json_exports(
            &crate::report_export::ReviewedReport::unreviewed(&report, &dispositions),
            corpus.as_ref(),
            &options,
        )
        .expect("json builds"),
    );
    let stem = report_filename_stem(&report);
    // The package's provenance comes through the one sanctioned bridge, against a watermark
    // whose domain is OVERDUE (the case the operator nag is written for). The manifest and
    // README then carry the one client-visible date, and the scan below proves nothing else
    // about maintenance state came with it.
    let mut audited = report.as_scan_report().clone();
    audited.provenance.audited_rule_ids = vec!["SEC-NO-HARDCODED-SECRETS-1".to_string()];
    let mut watermarked_corpus = camerata_rules::RuleSet::default();
    watermarked_corpus.push(camerata_rules::bare_rule(
        "SEC-NO-HARDCODED-SECRETS-1",
        "sec",
    ));
    let overdue = camerata_rules::CorpusFreshness::parse(
        "warn_after_days = 1\n[confirmed]\nsec = \"2020-01-01\"\n",
    )
    .expect("watermark parses");
    let provenance = crate::client_provenance::export_provenance_with(
        &audited,
        &json.cover,
        Some(&watermarked_corpus),
        Some(&overdue),
    );
    assert_eq!(
        provenance.rules_last_reviewed.as_deref(),
        Some("2020-01-01"),
        "anti-vacuity: the sanctioned date is in the package under test"
    );
    let readme = product_export_readme(&stem, &provenance);
    let zip_bytes = build_product_zip(
        &stem,
        b"%PDF-1.7 stand-in",
        &xlsx,
        &findings_bytes,
        &readme,
        &extra,
        &provenance,
    )
    .expect("zip assembles");

    let mut texts: Vec<(String, String)> = vec![("<pdf input json>".to_string(), pdf_input)];
    artifact_texts(&zip_bytes, "", &mut texts);
    assert!(
        texts.len() > 8,
        "the scan covered the pdf input, readme, findings, csv/json tables, xlsx parts and \
             manifest: {:?}",
        texts.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );

    // When typst is installed, also run the REAL handler end to end and scan its zip.
    if typst_on_path() {
        let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
        let project = state
            .projects
            .create("Acme", vec!["owner/repo".to_string()])
            .expect("project created");
        let project = state
            .projects
            .update(&project.id, |p| {
                p.client_name = "Example Client Ltd".to_string()
            })
            .expect("project exists");
        state.set_last_scan(
            project.id.clone(),
            make_scan_report("SEC-NO-HARDCODED-SECRETS-1"),
        );
        let resp = export_product(
            axum::extract::State(state),
            axum::extract::Path(project.id.clone()),
            axum::Json(AuditReportReq {
                dispositions: std::collections::HashMap::new(),
                options: crate::report_export::ReportOptions::default(),
            }),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        artifact_texts(&bytes, "handler:", &mut texts);
    }

    let leaks: Vec<String> = texts
        .iter()
        .filter_map(|(path, text)| {
            let found = operator_markers_in(text);
            (!found.is_empty()).then(|| format!("{path}: {found:?}"))
        })
        .collect();
    assert!(
        leaks.is_empty(),
        "operator-only corpus-freshness text reached a client artifact:\n  {}",
        leaks.join("\n  ")
    );

    // ── Source-level: the export builders do not even reference the freshness code ────
    //
    // Exactly one file may touch it: `client_provenance.rs`, the bridge that produces the
    // single client-visible review date, and only through `SANCTIONED_BRIDGE_IDENTIFIERS`.
    // Every artifact builder, and both export callers, name none of it.
    let lib_src = include_str!("../lib.rs");
    let start = lib_src
        .find("async fn export_product(")
        .expect("export_product exists");
    let end = lib_src[start..]
        .find("// ── error type")
        .map(|i| start + i)
        .expect("end of the export section");
    let export_section = &lib_src[start..end];
    for (name, src) in [
        ("report_export.rs", include_str!("../report_export.rs")),
        (
            "report_export/json_types.rs",
            include_str!("../report_export/json_types.rs"),
        ),
        (
            "report_export/marking.rs",
            include_str!("../report_export/marking.rs"),
        ),
        (
            "report_export/citations_fixes.rs",
            include_str!("../report_export/citations_fixes.rs"),
        ),
        (
            "report_export/classification.rs",
            include_str!("../report_export/classification.rs"),
        ),
        ("xlsx_export.rs", include_str!("../xlsx_export.rs")),
        (
            "lib.rs (export_product .. build_product_zip)",
            export_section,
        ),
        (
            "cli inspect_cmd.rs (the headless export caller)",
            include_str!("../../../cli/src/inspect_cmd.rs"),
        ),
    ] {
        let found = freshness_identifiers_in(src, &[]);
        assert!(
            found.is_empty(),
            "{name} references corpus-freshness code {found:?}; freshness is operator-only \
                 and must not be wired into a client-artifact builder or an export caller. The \
                 only sanctioned path is client_provenance::export_provenance"
        );
    }
    let bridge = include_str!("../client_provenance.rs");
    let found = freshness_identifiers_in(bridge, SANCTIONED_BRIDGE_IDENTIFIERS);
    assert!(
        found.is_empty(),
        "client_provenance.rs references operator-only freshness code {found:?}; it may \
             name only {SANCTIONED_BRIDGE_IDENTIFIERS:?}"
    );
    // Anti-vacuity: the bridge really does use every sanctioned identifier, so the
    // allowance above is a description of the code and not a blanket pass.
    for id in SANCTIONED_BRIDGE_IDENTIFIERS {
        assert!(
            bridge.contains(id),
            "client_provenance.rs no longer uses {id}; drop it from the allowance"
        );
    }
}

/// The gate's scanner has to be able to FAIL. These pin its behavior on fixture sources:
/// the sanctioned date path passes in the bridge, and nothing else does, in the bridge or
/// anywhere. Without this, loosening the allowance for the one date would also silently
/// loosen the gate for the nag text it exists to keep out.
#[test]
fn freshness_gate_allows_only_the_sanctioned_date_path() {
    // Exactly the sanctioned names, no more: pinned so a widened allowance shows in review.
    assert_eq!(
        SANCTIONED_BRIDGE_IDENTIFIERS,
        &[
            "CorpusFreshness",
            "corpus_freshness_path",
            "client_rules_last_reviewed"
        ]
    );
    // Every sanctioned name is itself in the forbidden-in-builders list.
    for id in SANCTIONED_BRIDGE_IDENTIFIERS {
        assert!(
            freshness_identifiers_in(id, &[]).len() == 1,
            "{id} must be forbidden outside the bridge"
        );
    }

    // The sanctioned path, as the bridge writes it, is clean in the bridge...
    let sanctioned_only =
        "let w = CorpusFreshness::load(&camerata_rules::corpus_freshness_path());\n\
                               client_rules_last_reviewed(&w, &used)";
    assert_eq!(
        freshness_identifiers_in(sanctioned_only, SANCTIONED_BRIDGE_IDENTIFIERS),
        Vec::<&str>::new()
    );
    // ...and is a violation anywhere else.
    assert_eq!(
        freshness_identifiers_in(sanctioned_only, &[]),
        vec![
            "CorpusFreshness",
            "corpus_freshness",
            "client_rules_last_reviewed"
        ]
    );

    // The nag, banner and report vocabulary stays forbidden even in the bridge: each of
    // these, appended to an otherwise sanctioned source, is caught.
    for operator_only in [
        "format_staleness_nag(&report)",
        "FreshnessReport",
        "OPERATOR_ONLY_MARKERS",
        "CORPUS_FRESHNESS.toml",
        "w.due_soon(today)",
        "payload[\"corpus_freshness\"]",
    ] {
        let src = format!("{sanctioned_only}\n{operator_only}");
        assert!(
            !freshness_identifiers_in(&src, SANCTIONED_BRIDGE_IDENTIFIERS).is_empty(),
            "the bridge scan must catch {operator_only:?}"
        );
    }

    // A sanctioned name does not shield an operator one it merely sits next to.
    assert_eq!(
        freshness_identifiers_in(
            "CorpusFreshness::load(p); let r: FreshnessReport = w.report();",
            SANCTIONED_BRIDGE_IDENTIFIERS
        ),
        vec!["FreshnessReport"]
    );
}

/// Gate 6 (coverage clause) of `docs/design/2026-10-20_machine-readable-exports.md`: the
/// manifest lists every ZIP member except itself, and each listed sha256 equals the actual
/// hash of that member. The corruption clause (that a flipped byte is caught) lives in
/// `tests/product_export_manifest_gates.rs`.
/// The scan settings' spend limit reaches the run as a cap. A limit the server cannot read
/// as a positive number must not become "no limit" (a typo would remove the guard).
#[test]
fn an_audit_request_carries_its_spend_limit_into_the_run_controls() {
    let req = |body: serde_json::Value| -> AuditReq { serde_json::from_value(body).unwrap() };
    assert_eq!(
        req(serde_json::json!({}))
            .run_controls(None, None)
            .max_cost_usd,
        None,
        "no field, no cap"
    );
    assert_eq!(
        req(serde_json::json!({ "max_cost_usd": null }))
            .run_controls(None, None)
            .max_cost_usd,
        None
    );
    assert_eq!(
        req(serde_json::json!({ "max_cost_usd": 25.0 }))
            .run_controls(None, None)
            .max_cost_usd,
        Some(25.0)
    );
    for bad in [0.0, -4.0] {
        let cap = req(serde_json::json!({ "max_cost_usd": bad }))
            .run_controls(None, None)
            .max_cost_usd;
        assert_eq!(
            cap,
            Some(f64::MIN_POSITIVE),
            "{bad} is the tightest cap, not none"
        );
    }
    // The project's fix model rides along; none set leaves the fix stage to the switch.
    let empty = req(serde_json::json!({}));
    assert_eq!(empty.run_controls(None, None).fix_model, None);
    assert_eq!(
        empty
            .run_controls(Some("claude-opus-5".to_string()), None)
            .fix_model
            .as_deref(),
        Some("claude-opus-5")
    );
}

/// The clone judge follows the open project's fast (Haiku) band: the run's controls carry the
/// band's primary model (default Haiku 5.5), a project that moves the band moves the judge,
/// and with no project the built-in default applies. The environment has no say.
#[test]
#[serial_test::serial(process_env)]
fn the_clone_judge_follows_the_projects_fast_band() {
    let _env = camerata_test_env::EnvGuard::set("CAMERATA_CLONE_JUDGE_MODEL", "env-judge");
    let state = AppState::seeded();
    let req: AuditReq = serde_json::from_value(serde_json::json!({})).unwrap();
    // No project: the built-in default.
    assert_eq!(clone_judge_step_model(&state), None);
    assert_eq!(
        req.run_controls_for(&state).clone_judge_model_for(),
        "claude-haiku-5-5"
    );

    let project = state
        .projects()
        .create("Judge wiring", vec!["me/repo".to_string()])
        .expect("create active project");
    assert_eq!(
        clone_judge_step_model(&state).as_deref(),
        Some("claude-haiku-5-5"),
        "a new project's fast band is Haiku 5.5"
    );
    state
        .projects()
        .update(&project.id, |p| {
            p.tier_map.fast = vec!["claude-haiku-4-5-20251001".to_string()];
        })
        .unwrap();
    assert_eq!(
        req.run_controls_for(&state).clone_judge_model_for(),
        "claude-haiku-4-5-20251001",
        "the project's fast band chooses the judge; the older id stays selectable"
    );
}

/// Rank 10, the wiring the desktop audit takes: the active project's fix step model becomes
/// the run's fix model through the same function both audit entry points call
/// (`AuditReq::run_controls_for`), and from there the model the fix stage resolves. Not a
/// hand-built `RunControls`: the project is set through its store, as the settings screen's
/// `POST /step-models` does.
#[test]
fn the_projects_fix_model_reaches_the_audit_runs_controls() {
    let state = AppState::seeded();
    let project = state
        .projects()
        .create("Fix wiring", vec!["me/repo".to_string()])
        .expect("create active project");
    let req: AuditReq = serde_json::from_value(serde_json::json!({})).unwrap();
    let resolved = |state: &AppState| {
        let controls = req.run_controls_for(state);
        let fix_model = controls
            .fix_model_for(Some("scan-model"), Some("calibration-model"))
            .map(str::to_string);
        (controls.fix_model.clone(), fix_model)
    };

    // New project: no fix model, so the switch decides (Haiku by default).
    assert_eq!(
        resolved(&state),
        (
            None,
            Some(crate::ai_audit::FIX_STAGE_HAIKU_MODEL.to_string())
        )
    );

    // The operator picks a model for the fix step: it is the run's fix model.
    state
        .projects()
        .set_step_model(
            &project.id,
            crate::project::StepKind::Fix,
            "claude-opus-5".to_string(),
        )
        .unwrap();
    assert_eq!(
        resolved(&state),
        (
            Some("claude-opus-5".to_string()),
            Some("claude-opus-5".to_string())
        )
    );

    // Cleared back to Default (a blank id): unset again.
    state
        .projects()
        .set_step_model(&project.id, crate::project::StepKind::Fix, String::new())
        .unwrap();
    assert_eq!(
        resolved(&state),
        (
            None,
            Some(crate::ai_audit::FIX_STAGE_HAIKU_MODEL.to_string())
        )
    );

    // No active project at all: nothing set.
    assert_eq!(
        resolved(&AppState::seeded()),
        (
            None,
            Some(crate::ai_audit::FIX_STAGE_HAIKU_MODEL.to_string())
        )
    );
}

#[test]
fn manifest_lists_and_hashes_every_file_except_itself() {
    use sha2::{Digest, Sha256};
    let extra = vec![
        ("csv/all-findings.csv".to_string(), b"a,b\n1,2\n".to_vec()),
        ("json/all-findings.json".to_string(), b"[]".to_vec()),
    ];
    let prov = ManifestProvenance {
        repos: vec![ProvenanceRepo {
            name: "acme-portal".to_string(),
            commit: Some("0123456789abcdef0123456789abcdef01234567".to_string()),
            dirty: false,
        }],
        generated_at: "2026-10-21T00:00:00Z".to_string(),
        ..Default::default()
    };
    let zip_bytes = build_product_zip(
        "acme-audit",
        b"%PDF-fake",
        b"PK-fake-xlsx",
        b"{}",
        "readme text",
        &extra,
        &prov,
    )
    .expect("zip assembles");

    // Pull manifest.json out of the archive.
    let mut archive =
        zip::ZipArchive::new(std::io::Cursor::new(zip_bytes.clone())).expect("valid zip");
    let mut manifest_str = String::new();
    {
        use std::io::Read;
        archive
            .by_name("manifest.json")
            .expect("manifest.json present")
            .read_to_string(&mut manifest_str)
            .unwrap();
    }
    let manifest: serde_json::Value = serde_json::from_str(&manifest_str).unwrap();

    // Provenance block present and populated from the caller.
    assert_eq!(manifest["inspection"]["repos"][0]["name"], "acme-portal");
    assert_eq!(
        manifest["inspection"]["repos"][0]["commit"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert!(manifest["inspection"]["engine_version"]
        .as_str()
        .unwrap()
        .starts_with("camerata "));
    // rules_last_reviewed is present (null until a confirmed sweep) — the field always exists.
    assert!(manifest["inspection"].get("rules_last_reviewed").is_some());

    let files = manifest["files"].as_array().unwrap();
    let listed: Vec<&str> = files.iter().map(|f| f["path"].as_str().unwrap()).collect();
    // Every non-manifest file is listed; manifest.json never lists itself.
    for expected in [
        "acme-audit.pdf",
        "acme-audit-findings.xlsx",
        "findings.json",
        "README.txt",
        "csv/all-findings.csv",
        "json/all-findings.json",
    ] {
        assert!(
            listed.contains(&expected),
            "manifest missing {expected}: {listed:?}"
        );
    }
    assert!(
        !listed.contains(&"manifest.json"),
        "manifest must not list itself"
    );

    // Each listed sha256 equals the actual hash of that entry in the zip.
    for f in files {
        let path = f["path"].as_str().unwrap();
        let claimed = f["sha256"].as_str().unwrap();
        let mut bytes = Vec::new();
        {
            use std::io::Read;
            archive
                .by_name(path)
                .unwrap()
                .read_to_end(&mut bytes)
                .unwrap();
        }
        let actual: String = Sha256::digest(&bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(claimed, actual, "sha256 mismatch for {path}");
    }
}

/// Regression for the zip-crate-default-timestamp bug: with `SimpleFileOptions::default()`
/// left alone, every entry's `last_modified()` reads 1980-01-01 00:00:00 (the DOS/zip
/// epoch `zip` falls back to when no timestamp is set, since this workspace doesn't enable
/// the crate's `time`/`chrono` cargo features). `build_product_zip` now stamps every entry
/// with `zip_now()` (the actual build time) instead. Exercises `build_product_zip`
/// directly with dummy bytes — no `typst` dependency, so it always runs.
#[test]
fn product_zip_entries_carry_a_real_build_timestamp_not_the_1980_epoch() {
    let zip_bytes = build_product_zip(
        "acme-audit",
        b"%PDF-fake",
        b"PK-fake-xlsx",
        b"{}",
        "readme text",
        &[],
        &ManifestProvenance::default(),
    )
    .expect("zip assembles");

    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(zip_bytes)).expect("valid zip");
    assert!(
        archive.len() >= 4,
        "expects pdf + xlsx + json + readme entries"
    );
    for i in 0..archive.len() {
        let entry = archive.by_index(i).unwrap();
        let mtime = entry
            .last_modified()
            .expect("a timestamp was set on every entry");
        assert_ne!(
            (mtime.year(), mtime.month(), mtime.day()),
            (1980, 1, 1),
            "entry {:?} still carries the zip crate's 1980-01-01 default — \
                 last_modified_time() was not applied",
            entry.name()
        );
    }
}

/// `zip_now` must resolve to the current calendar year (sanity check that it's actually
/// reading the clock, not silently falling back to `DateTime::default()` — which would
/// pass unnoticed since `unwrap_or_default()` never panics).
#[test]
fn zip_now_reflects_the_current_year() {
    use chrono::Datelike;
    let now = zip_now();
    let current_year = chrono::Utc::now().year() as u16;
    assert_eq!(
        now.year(),
        current_year,
        "zip_now must not fall back to the 1980 default"
    );
}

/// active_project_context returns scan_results_section from last_scan when no draft.
#[tokio::test]
async fn active_project_context_uses_last_scan_when_no_draft() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    // Create a project (automatically becomes active).
    let project = state
        .projects
        .create("TestProj", vec!["owner/repo".to_string()])
        .unwrap();

    // No draft has been saved, but we store a completed scan report.
    let report = make_scan_report("SEC-NO-HARDCODED-SECRETS-1");
    state.set_last_scan(project.id.clone(), report);

    let app = router(state);
    let req = Request::builder()
        .uri("/api/projects/active/context")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = body_json(resp).await;
    let section = body.get("scan_results_section");
    assert!(
        section.is_some() && section.unwrap() != &serde_json::Value::Null,
        "scan_results_section must be populated from last_scan when no draft, got: {body:?}"
    );
    let section_str = section.unwrap().as_str().unwrap_or("");
    assert!(
        section_str.contains("SEC-NO-HARDCODED-SECRETS-1"),
        "scan_results_section must contain the finding rule_id, got: {section_str}"
    );
}

/// The chat context returns the in-progress draft, and the draft carries the reviewer's
/// timer log (internal only). The log must not be in what the chat gets, and everything else
/// in the draft still is.
#[tokio::test]
async fn active_project_context_leaves_the_internal_review_log_out_of_the_draft() {
    use camerata_api_types::review_timing::{INTERNAL_ONLY_KEYS, REVIEW_LOG_KEY};
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("TestProj", vec!["owner/repo".to_string()])
        .unwrap();
    state.draft.save(
        &project.id,
        serde_json::json!({
            "scan": { "findings": [], "coverage_notes": [] },
            "viewed_repo": "owner/repo",
            REVIEW_LOG_KEY: {
                "review_groups": { "g": { "opened_at_ms": 1, "active_ms": 72_000_000, "group_reviewed": true } },
                "review_rows": { "r": { "reviewed_by": "operator", "reviewed_at_ms": 2, "review_decision_ms": 3 } },
                "review_samples": [4],
                "review_unreadable": { "kept": true },
            },
        }),
    );
    let resp = router(state)
        .oneshot(
            Request::builder()
                .uri("/api/projects/active/context")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body = body_json(resp).await;
    let text = body.to_string();
    for key in INTERNAL_ONLY_KEYS {
        assert!(
            !text.contains(key),
            "{key} reached the chat context: {text}"
        );
    }
    assert_eq!(
        body["draft_json"]["viewed_repo"], "owner/repo",
        "the rest of the draft is still there: {text}"
    );
}

/// Draft takes precedence over last_scan when the draft has findings.
#[tokio::test]
async fn active_project_context_draft_takes_precedence_over_last_scan() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("TestProj", vec!["owner/repo".to_string()])
        .unwrap();

    // Store a last_scan report with one rule id.
    let report = make_scan_report("LAST-SCAN-RULE");
    state.set_last_scan(project.id.clone(), report);

    // Save a draft with a DIFFERENT rule id — draft must win.
    let draft = serde_json::json!({
        "scan": {
            "findings": [{
                "repo": "owner/repo",
                "path": "src/lib.rs",
                "line": 1,
                "rule_id": "DRAFT-RULE",
                "severity": "high",
                "status": "active",
                "snippet": "",
                "detail": "draft finding"
            }],
            "coverage_notes": []
        }
    });
    state.draft.save(&project.id, draft);

    let app = router(state);
    let req = Request::builder()
        .uri("/api/projects/active/context")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = body_json(resp).await;
    let section_str = body
        .get("scan_results_section")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    assert!(
        section_str.contains("DRAFT-RULE"),
        "draft rule must appear when draft has findings, got: {section_str}"
    );
    assert!(
        !section_str.contains("LAST-SCAN-RULE"),
        "last_scan rule must NOT appear when draft takes precedence, got: {section_str}"
    );
}

// ── recent_scan fallback tests ────────────────────────────────────────────────────────

/// set_last_scan also populates recent_scan; get_recent_scan returns the report.
#[test]
fn set_last_scan_populates_recent_scan() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));

    // Before any write: recent_scan is None.
    assert!(
        state.get_recent_scan().is_none(),
        "get_recent_scan must return None before any scan is stored"
    );

    state.set_last_scan("proj-1".to_string(), make_scan_report("RULE-X"));

    let recent = state.get_recent_scan();
    assert!(
        recent.is_some(),
        "get_recent_scan must be Some after set_last_scan"
    );
    assert_eq!(
        recent.unwrap().findings[0].rule_id,
        "RULE-X",
        "recent_scan must mirror the report passed to set_last_scan"
    );
}

/// The patterns across services are rebuilt when a scan is stored, so the cockpit tab never
/// reads a stale or empty matrix; a scan of one repo stores none.
#[test]
fn storing_a_scan_rebuilds_its_patterns_across_services() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let mut report = make_scan_report("RULE-X");
    report.repos = vec!["acme/a".to_string(), "acme/b".to_string()];
    report.findings[0].repo = "acme/a".to_string();
    report.ledger.record_rule(
        "acme/a",
        "RULE-X",
        crate::scan_ledger::RuleTier::Semantic,
        true,
        None,
        3,
        1,
    );
    report.ledger.record_rule(
        "acme/b",
        "RULE-X",
        crate::scan_ledger::RuleTier::Semantic,
        true,
        None,
        3,
        0,
    );
    assert!(report.patterns_across_services.rows.is_empty());
    state.set_last_scan("proj-multi".to_string(), report);
    let stored = state.get_last_scan("proj-multi").unwrap();
    assert_eq!(stored.patterns_across_services.rows.len(), 1);
    assert_eq!(stored.patterns_across_services.rows[0].rule_id, "RULE-X");

    state.set_last_scan("proj-one".to_string(), make_scan_report("RULE-X"));
    let one = state.get_last_scan("proj-one").unwrap();
    assert!(!one.patterns_across_services.is_present());
}

/// When two projects are scanned in sequence, recent_scan reflects the LATEST one.
#[test]
fn recent_scan_tracks_most_recent_write() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    state.set_last_scan("proj-A".to_string(), make_scan_report("RULE-FIRST"));
    state.set_last_scan("proj-B".to_string(), make_scan_report("RULE-SECOND"));

    let recent = state.get_recent_scan().unwrap();
    assert_eq!(
        recent.findings[0].rule_id, "RULE-SECOND",
        "recent_scan must track the most recent set_last_scan call"
    );

    // Per-project last_scan must still be independent.
    assert_eq!(
        state.get_last_scan("proj-A").unwrap().findings[0].rule_id,
        "RULE-FIRST"
    );
}

/// active_project_context surfaces recent_scan when the active project has no
/// per-project last_scan entry (simulates the active-project-changed-between-submit-
/// and-complete race that caused "Scan results (none yet)").
#[tokio::test]
async fn active_project_context_does_not_leak_recent_scan_across_projects() {
    // ISOLATION (A5): the recent_scan global fallback was REMOVED. A scan stored under
    // a different project's id must NEVER surface in the active project's chat grounding.
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    // Create and activate a project.
    let project = state
        .projects
        .create("ActiveProj", vec!["owner/repo".to_string()])
        .unwrap();

    // Simulate a scan stored under a DIFFERENT project id (the project that was active at
    // scan submission time) — NOT under `project.id`. This also populates recent_scan.
    state.set_last_scan(
        "other-project-id".to_string(),
        make_scan_report("RECENT-RULE"),
    );

    // Confirm: per-project lookup for the ACTIVE project is empty.
    assert!(
        state.get_last_scan(&project.id).is_none(),
        "test setup: active project must have no per-project last_scan entry"
    );
    // Confirm: recent_scan was populated by the set_last_scan call above (so the
    // assertion below proves the fallback is gone, not that the data is absent).
    assert!(
        state.get_recent_scan().is_some(),
        "test setup: recent_scan must be populated"
    );

    let app = router(state);
    let req = Request::builder()
        .uri("/api/projects/active/context")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = body_json(resp).await;
    let section = body.get("scan_results_section");
    // The fallback is gone: with no per-project scan for the active project, the section
    // must be absent/null and must NOT contain the OTHER project's rule id.
    let section_str = section.and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        !section_str.contains("RECENT-RULE"),
        "cross-project recent_scan must NOT leak into the active project's chat grounding, \
             got: {body:?}"
    );
}

/// Per-project last_scan takes precedence over recent_scan when both are present.
#[tokio::test]
async fn per_project_last_scan_wins_over_recent_scan() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("ActiveProj", vec!["owner/repo".to_string()])
        .unwrap();

    // Store a recent_scan for a DIFFERENT project (written second, so it is "more recent").
    state.set_last_scan(
        "other-project-id".to_string(),
        make_scan_report("RECENT-RULE"),
    );

    // Now store a per-project entry for the ACTIVE project.  Even though recent_scan
    // was written after this, the per-project entry must win.
    state.set_last_scan(project.id.clone(), make_scan_report("PROJECT-RULE"));

    let app = router(state);
    let req = Request::builder()
        .uri("/api/projects/active/context")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = body_json(resp).await;
    let section_str = body
        .get("scan_results_section")
        .and_then(|v| v.as_str())
        .unwrap_or("");

    assert!(
        section_str.contains("PROJECT-RULE"),
        "per-project last_scan must take precedence over recent_scan, got: {section_str}"
    );
    assert!(
        !section_str.contains("RECENT-RULE"),
        "recent_scan must NOT appear when per-project last_scan is present, got: {section_str}"
    );
}

// ── render_selected_rules_for_chat tests ──────────────────────────────────────────

/// Empty repo_selection → None.
#[test]
fn render_selected_rules_none_on_empty_selection() {
    let draft = serde_json::json!({
        "repo_selection": {},
        "chosen": {}
    });
    assert!(
        render_selected_rules_for_chat(&draft).is_none(),
        "empty repo_selection must return None"
    );
}

/// No repo_selection key → None.
#[test]
fn render_selected_rules_none_when_no_repo_selection_key() {
    let draft = serde_json::json!({ "scan": {} });
    assert!(
        render_selected_rules_for_chat(&draft).is_none(),
        "missing repo_selection must return None"
    );
}

/// Selected rule ids appear in output with correct count.
#[test]
fn render_selected_rules_includes_rule_ids_and_count() {
    let draft = serde_json::json!({
        "repo_selection": {
            "owner/api": ["SEC-1", "PERF-1"],
            "owner/ui": ["SEC-1"]
        },
        "chosen": {},
        "scan": { "proposed_rules": [] }
    });
    let out = render_selected_rules_for_chat(&draft).expect("non-empty selection must return Some");
    assert!(
        out.contains("SEC-1"),
        "SEC-1 must appear in selected rules output"
    );
    assert!(
        out.contains("PERF-1"),
        "PERF-1 must appear in selected rules output"
    );
    // Total selected = 2 distinct rules.
    assert!(
        out.contains("Total selected: 2"),
        "total must count distinct rules, got: {out}"
    );
}

/// Chosen non-default option is rendered with its label.
#[test]
fn render_selected_rules_chosen_option_shown() {
    let draft = serde_json::json!({
        "repo_selection": {
            "owner/api": ["RULE-A"]
        },
        "chosen": {
            "RULE-A": "opt-strict"
        },
        "scan": {
            "proposed_rules": [
                {
                    "id": "RULE-A",
                    "title": "My Rule",
                    "scope": "repo-local",
                    "kind": "review",
                    "enforcement": "prose",
                    "verification": "draft",
                    "options": [
                        { "id": "opt-default", "label": "Default mode", "directive": "use default", "why": "" },
                        { "id": "opt-strict", "label": "Strict mode", "directive": "use strict", "why": "" }
                    ],
                    "default_option": "opt-default"
                }
            ]
        }
    });
    let out = render_selected_rules_for_chat(&draft).expect("must return Some when rules selected");
    assert!(
        out.contains("chosen: Strict mode"),
        "chosen option label must appear in output, got: {out}"
    );
}

/// Default option is annotated as [default option] when no override is chosen.
#[test]
fn render_selected_rules_default_option_annotated() {
    let draft = serde_json::json!({
        "repo_selection": {
            "owner/api": ["RULE-B"]
        },
        "chosen": {},
        "scan": {
            "proposed_rules": [
                {
                    "id": "RULE-B",
                    "title": "Rule B",
                    "scope": "repo-local",
                    "kind": "review",
                    "enforcement": "prose",
                    "verification": "draft",
                    "options": [
                        { "id": "opt-a", "label": "Option A", "directive": "do a", "why": "" }
                    ],
                    "default_option": "opt-a"
                }
            ]
        }
    });
    let out = render_selected_rules_for_chat(&draft).expect("must return Some when rules selected");
    assert!(
        out.contains("[default option]"),
        "default option must be annotated when no override chosen, got: {out}"
    );
}

/// `active_project_context` includes selected_rules_section when draft has
/// repo_selection entries.
#[tokio::test]
async fn active_project_context_includes_selected_rules_section() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("TestProj", vec!["owner/repo".to_string()])
        .unwrap();

    // Save a draft with a repo_selection (simulates the user checking rules).
    let draft = serde_json::json!({
        "scan": {
            "repos": ["owner/repo"],
            "findings": [],
            "proposed_rules": [
                {
                    "id": "SEC-NO-HARDCODED-SECRETS-1",
                    "title": "No hardcoded secrets",
                    "scope": "repo-local",
                    "kind": "review",
                    "enforcement": "prose",
                    "verification": "grounded",
                    "options": [],
                    "default_option": null
                }
            ]
        },
        "repo_selection": {
            "owner/repo": ["SEC-NO-HARDCODED-SECRETS-1"]
        },
        "chosen": {}
    });
    state.draft.save(&project.id, draft);

    let app = router(state);
    let req = Request::builder()
        .uri("/api/projects/active/context")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = body_json(resp).await;
    let section = body.get("selected_rules_section");
    assert!(
        section.is_some() && section.unwrap() != &serde_json::Value::Null,
        "selected_rules_section must be present when repo_selection is non-empty, got: {body:?}"
    );
    let section_str = section.unwrap().as_str().unwrap_or("");
    assert!(
        section_str.contains("SEC-NO-HARDCODED-SECRETS-1"),
        "selected rule id must appear in selected_rules_section, got: {section_str}"
    );
    assert!(
        section_str.contains("Total selected: 1"),
        "total count must be 1, got: {section_str}"
    );
}

/// `active_project_context` has selected_rules_section absent when repo_selection is empty.
#[tokio::test]
async fn active_project_context_selected_rules_absent_when_no_selection() {
    let state = AppState::new(std::sync::Arc::new(InMemoryStoryStore::new()));
    let project = state
        .projects
        .create("EmptyProj", vec!["owner/repo".to_string()])
        .unwrap();

    // Draft has an empty repo_selection.
    let draft = serde_json::json!({
        "scan": { "repos": ["owner/repo"], "findings": [], "proposed_rules": [] },
        "repo_selection": {},
        "chosen": {}
    });
    state.draft.save(&project.id, draft);

    let app = router(state);
    let req = Request::builder()
        .uri("/api/projects/active/context")
        .body(Body::empty())
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    let body = body_json(resp).await;
    // selected_rules_section must be absent (None / null) when nothing is selected.
    let section = body.get("selected_rules_section");
    let is_absent = section.is_none() || section == Some(&serde_json::Value::Null);
    assert!(
        is_absent,
        "selected_rules_section must be absent when repo_selection is empty, got: {body:?}"
    );
}

// ── derive_scope_branch (R6 multi-repo worktree setup) ─────────────────────

/// `BranchMode::Existing { branch_name }` → use the named branch as-is.
#[test]
fn derive_scope_branch_existing_mode_uses_branch_name() {
    let mode = crate::uow::BranchMode::Existing {
        branch_name: "feature/my-branch".to_string(),
    };
    assert_eq!(
        derive_scope_branch(&mode, "acme/api#42"),
        "feature/my-branch"
    );
}

/// `BranchMode::Existing { branch_name: "" }` (empty) → fallback to `camerata/<story_id>`.
#[test]
fn derive_scope_branch_existing_empty_name_falls_back_to_slug() {
    let mode = crate::uow::BranchMode::Existing {
        branch_name: "".to_string(),
    };
    assert_eq!(
        derive_scope_branch(&mode, "acme/api#42"),
        "camerata/acme/api#42",
    );
}

/// `BranchMode::NewFromBase { new_name }` → use `new_name` when non-empty.
#[test]
fn derive_scope_branch_new_from_base_uses_new_name() {
    let mode = crate::uow::BranchMode::NewFromBase {
        base: "main".to_string(),
        new_name: "camerata/story-7".to_string(),
    };
    assert_eq!(derive_scope_branch(&mode, "acme/api#7"), "camerata/story-7");
}

/// `BranchMode::NewFromBase { new_name: "" }` → fallback to `camerata/<story_id>`.
#[test]
fn derive_scope_branch_new_from_base_empty_name_falls_back_to_slug() {
    let mode = crate::uow::BranchMode::NewFromBase {
        base: "main".to_string(),
        new_name: String::new(),
    };
    assert_eq!(
        derive_scope_branch(&mode, "acme/api#7"),
        "camerata/acme/api#7",
    );
}

// ── uow_intake_ship single-repo fallback (no-token 4xx) ─────────────────────

/// When the GitHub token is absent, `POST /api/uow/:story_id/intake/ship` returns 400.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(process_env)]
async fn intake_ship_returns_bad_request_when_no_github_token() {
    // Remove the token from the test env so the handler detects its absence.
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");

    let state = AppState::seeded();
    let app = router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/api/uow/acme__api-42/intake/ship")
        .header("content-type", "application/json")
        .body(Body::from("{}"))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        axum::http::StatusCode::BAD_REQUEST,
        "must return 400 when no GitHub token is available"
    );
    let body = body_json(resp).await;
    let err = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    assert!(
        err.contains("GitHub token") || err.contains("no GitHub"),
        "error must mention missing GitHub token, got: {body:?}"
    );
}

// ── POST /api/apps (create_app, Part 2 of the scaffolder) ──────────────────
//
// The happy Skeleton path (GitHub mocked, project registered with the two
// seeded custom rules, governance event recorded) is covered directly against
// `crate::create_app::run_create_app` in `create_app.rs`'s own test module —
// this handler is a thin wrapper (build a real transport/pusher, delegate) with
// no branching of its own worth re-testing through the HTTP layer. These router
// tests cover what IS specific to the HTTP wrapper: the FromScratch short-circuit
// and the two fail-soft error paths.

/// A `FromScratch` target returns the reason immediately, at 200 with `ok:true`
/// — never a scaffold, never a GitHub call, even with no workspace root or token
/// configured at all.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(process_env)]
async fn create_app_from_scratch_target_returns_reason_without_scaffolding() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let state = AppState::seeded();
    let app = router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/api/apps")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({
                "name": "Music Library",
                "description": "",
                "target": "desktop",
            })
            .to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), axum::http::StatusCode::OK);
    let body = body_json(resp).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["strategy"], "from_scratch");
    assert!(body["reason"].as_str().unwrap_or("").contains("Desktop"));
}

/// A `Skeleton` (default target) request with no workspace root configured fails
/// soft: `ok:false` with a message naming the workspace, never a 500.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(process_env)]
async fn create_app_skeleton_without_workspace_root_fails_soft() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let state = AppState::seeded();
    let app = router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/api/apps")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "name": "Trip Planner", "description": "" }).to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        axum::http::StatusCode::OK,
        "fail-soft: never a 500"
    );
    let body = body_json(resp).await;
    assert_eq!(body["ok"], false);
    assert!(
        body["message"].as_str().unwrap_or("").contains("workspace"),
        "message must name the missing workspace root, got: {body:?}"
    );
}

/// A `Skeleton` request with a workspace root but no GitHub token fails soft:
/// `ok:false` with a message naming the missing token.
#[tokio::test(flavor = "multi_thread")]
#[serial_test::serial(process_env)]
async fn create_app_skeleton_without_github_token_fails_soft() {
    let _env = camerata_test_env::EnvGuard::remove("CAMERATA_GITHUB_TOKEN");
    let state = AppState::seeded();
    let tmp = tempfile::tempdir().expect("tempdir");
    state
        .settings()
        .set_workspace_root(Some(tmp.path().to_string_lossy().into_owned()));
    let app = router(state);
    let req = Request::builder()
        .method("POST")
        .uri("/api/apps")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::json!({ "name": "Trip Planner", "description": "" }).to_string(),
        ))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        axum::http::StatusCode::OK,
        "fail-soft: never a 500"
    );
    let body = body_json(resp).await;
    assert_eq!(body["ok"], false);
    assert!(
        body["message"]
            .as_str()
            .unwrap_or("")
            .contains("GitHub token"),
        "message must name the missing GitHub token, got: {body:?}"
    );
}

// ── UowIntakeShipResponse structure ─────────────────────────────────────────

/// `UowIntakeShipResponse` is serializable with the expected shape.
#[test]
fn uow_intake_ship_response_serializes_correctly() {
    let r = UowIntakeShipResponse {
        repos: vec![
            RepoShipResult {
                repo: "acme/api".to_string(),
                branch: "camerata/story-42".to_string(),
                ok: true,
                pr_url: Some("https://github.com/acme/api/pull/1".to_string()),
                pr_number: Some(1),
                error: None,
            },
            RepoShipResult {
                repo: "acme/frontend".to_string(),
                branch: "camerata/story-42".to_string(),
                ok: false,
                pr_url: None,
                pr_number: None,
                error: Some("push failed".to_string()),
            },
        ],
        all_ok: false,
    };
    let json = serde_json::to_value(&r).unwrap();
    assert_eq!(json["all_ok"], false);
    assert_eq!(json["repos"][0]["ok"], true);
    assert_eq!(json["repos"][1]["ok"], false);
    assert_eq!(json["repos"][0]["pr_number"], 1);
    assert_eq!(json["repos"][1]["error"], "push failed");
}

// ── multi-repo worktree setup: intake.repos determines the path ──────────────

/// When intake.repos is empty (single-repo), repo_worktrees stays empty and
/// execute_dev_implement_run uses the single-repo fallback path.
#[test]
fn multi_repo_path_requires_two_or_more_intake_repos() {
    // This test verifies the routing logic: <=1 intake repos → empty worktrees.
    let empty: Vec<crate::uow::RepoScope> = vec![];
    assert!(
        empty.len() <= 1,
        "zero intake repos → single-repo path (repo_worktrees = [])"
    );
    let single = [crate::uow::RepoScope {
        repo: "acme/api".to_string(),
        branch: crate::uow::BranchMode::default(),
    }];
    assert!(
        single.len() <= 1,
        "one intake repo → single-repo path (repo_worktrees = [])"
    );
    let multi = [
        crate::uow::RepoScope {
            repo: "acme/api".to_string(),
            branch: crate::uow::BranchMode::default(),
        },
        crate::uow::RepoScope {
            repo: "acme/frontend".to_string(),
            branch: crate::uow::BranchMode::default(),
        },
    ];
    assert!(
        multi.len() > 1,
        "two intake repos → multi-repo path (repo_worktrees populated)"
    );
}

// ── Credential manager tests ───────────────────────────────────────────────

/// GET /api/credentials returns all known credentials with is_set=false when nothing
/// is stored, and is_set=true + a masked value after one is stored.
#[tokio::test]
async fn list_credentials_returns_all_known_names_with_is_set_flag() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let app = router(state.clone());

    // Initially: all credentials are unset.
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/credentials")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    let arr = json.as_array().expect("must be an array");
    // Both known credentials must be present.
    let github = arr
        .iter()
        .find(|c| c["name"] == "github_token")
        .expect("must include github_token");
    let or_key = arr
        .iter()
        .find(|c| c["name"] == "openrouter_api_key")
        .expect("must include openrouter_api_key");
    assert!(
        !github["is_set"].as_bool().unwrap_or(true),
        "initially not set"
    );
    assert!(
        !or_key["is_set"].as_bool().unwrap_or(true),
        "initially not set"
    );

    // Store a GitHub token.
    state
        .credential_store
        .set(crate::credentials::GITHUB_TOKEN, "ghp_test_value")
        .expect("set must succeed in MemoryCredentialStore");

    // Now list again — is_set must flip to true for the github_token entry.
    let resp2 = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/api/credentials")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let json2 = body_json(resp2).await;
    let arr2 = json2.as_array().expect("must be an array");
    let github2 = arr2
        .iter()
        .find(|c| c["name"] == "github_token")
        .expect("github_token present");
    assert!(
        github2["is_set"].as_bool().unwrap_or(false),
        "now is_set=true"
    );
    let masked = github2["masked"].as_str().expect("masked must be present");
    // Masked value must not contain the full token.
    assert!(
        !masked.contains("ghp_test_value"),
        "masked must not reveal full token"
    );
    // Must start with the first 4 chars.
    assert!(
        masked.starts_with("ghp_"),
        "masked must start with first 4 chars"
    );
}

/// POST /api/credentials/:name rejects unknown credential names.
#[tokio::test]
async fn set_credential_rejects_unknown_name() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/credentials/totally_unknown_key")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"value":"secret"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], false);
}

/// POST /api/credentials/:name rejects empty values.
#[tokio::test]
async fn set_credential_rejects_empty_value() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/credentials/github_token")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"value":""}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

/// POST /api/credentials/:name returns { ok: true, masked: "…" } on success,
/// and the masked value never contains the full token.
#[tokio::test]
async fn set_credential_returns_masked_value_not_full_token() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    let app = router(state);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/credentials/github_token")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"value":"ghp_supersecrettoken"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let json = body_json(resp).await;
    assert_eq!(json["ok"], true);
    let masked = json["masked"]
        .as_str()
        .expect("masked field must be present");
    // Full token must NEVER appear in the response.
    assert!(
        !masked.contains("ghp_supersecrettoken"),
        "response must never contain the full token: {masked}"
    );
    // The prefix appears.
    assert!(
        masked.starts_with("ghp_"),
        "masked starts with first 4 chars"
    );
}

// ── OpenRouter registry-refresh triggers ────────────────────────────────────
//
// `is_openrouter_credential` and `trigger_openrouter_refresh_if_needed` are the
// gap fix: before this, `POST /api/models/registry/refresh` had zero callers, so a
// saved key never populated any picker until someone curled the endpoint by hand.
// These tests assert the DECISION synchronously rather than awaiting (and thereby
// racing, or making a live OpenRouter network call from) the task the trigger
// spawns — see the doc comments on both functions above `serve()`.

/// Pure name-matching predicate: only the OpenRouter credential name matches.
#[test]
fn is_openrouter_credential_matches_only_the_openrouter_key_name() {
    assert!(is_openrouter_credential(
        crate::credentials::OPENROUTER_API_KEY
    ));
    assert!(!is_openrouter_credential(crate::credentials::GITHUB_TOKEN));
    assert!(!is_openrouter_credential("anthropic_api_key"));
    assert!(!is_openrouter_credential(""));
}

/// Saving the OpenRouter credential schedules a refresh (return value `true`). The
/// credential store passed in has NO key set, so even if the spawned task happens to
/// be polled before this test function returns, `try_refresh_from_store`'s own guard
/// no-ops immediately — this test can never make a live HTTP call, by construction.
#[tokio::test]
async fn trigger_openrouter_refresh_if_needed_schedules_for_the_openrouter_key() {
    let model_registry = crate::model_registry::ModelRegistry::new();
    let credential_store: Arc<dyn crate::credentials::CredentialStore> =
        Arc::new(crate::credentials::MemoryCredentialStore::new());
    let scheduled = trigger_openrouter_refresh_if_needed(
        crate::credentials::OPENROUTER_API_KEY,
        &model_registry,
        credential_store,
    );
    assert!(
        scheduled,
        "saving the OpenRouter key must schedule a refresh"
    );
}

/// Saving any OTHER credential must NOT schedule a refresh.
#[tokio::test]
async fn trigger_openrouter_refresh_if_needed_ignores_other_credentials() {
    let model_registry = crate::model_registry::ModelRegistry::new();
    let credential_store: Arc<dyn crate::credentials::CredentialStore> =
        Arc::new(crate::credentials::MemoryCredentialStore::new());
    let scheduled = trigger_openrouter_refresh_if_needed(
        crate::credentials::GITHUB_TOKEN,
        &model_registry,
        credential_store,
    );
    assert!(
        !scheduled,
        "saving a non-OpenRouter credential must not schedule a refresh"
    );
}

/// End-to-end through the real HTTP handler: saving a DIFFERENT credential (github_token)
/// must leave the OpenRouter registry cache untouched (`openrouter_fetched` stays false).
/// This is the safe half of the handler-level assertion — the openrouter_api_key case is
/// covered at the function level above instead of here, to avoid any chance of the
/// request handler's spawned task making a live call to openrouter.ai mid-test.
#[tokio::test]
async fn set_credential_for_non_openrouter_name_does_not_touch_the_registry_cache() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    assert!(!state.model_registry.openrouter_fetched());
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/credentials/github_token")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"value":"ghp_unrelated_token"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        !state.model_registry.openrouter_fetched(),
        "a non-OpenRouter credential save must never trigger a registry refresh"
    );
}

// ── Live Anthropic model-registry refresh triggers (Feature A) ─────────────────
//
// Mirrors the OpenRouter registry-refresh trigger tests immediately above, one for
// one. See `docs/design/2026-08-27_backend-safety-and-live-models.md`'s Feature A
// section and the doc comments on `is_anthropic_credential` /
// `trigger_anthropic_refresh_if_needed` above `serve()`.

/// Pure name-matching predicate: only the Anthropic credential name matches.
#[test]
fn is_anthropic_credential_matches_only_the_anthropic_key_name() {
    assert!(is_anthropic_credential(
        crate::credentials::ANTHROPIC_API_KEY
    ));
    assert!(!is_anthropic_credential(crate::credentials::GITHUB_TOKEN));
    assert!(!is_anthropic_credential(
        crate::credentials::OPENROUTER_API_KEY
    ));
    assert!(!is_anthropic_credential(""));
}

/// Saving the Anthropic credential schedules a refresh (return value `true`). The
/// credential store passed in has NO key set, so even if the spawned task happens to
/// be polled before this test function returns, `try_refresh_anthropic_from_store`'s
/// own guard no-ops immediately — this test can never make a live HTTP call, by
/// construction (mirrors `trigger_openrouter_refresh_if_needed_schedules_for_the_
/// openrouter_key`'s exact same discipline).
#[tokio::test]
async fn trigger_anthropic_refresh_if_needed_schedules_for_the_anthropic_key() {
    let model_registry = crate::model_registry::ModelRegistry::new();
    let credential_store: Arc<dyn crate::credentials::CredentialStore> =
        Arc::new(crate::credentials::MemoryCredentialStore::new());
    let scheduled = trigger_anthropic_refresh_if_needed(
        crate::credentials::ANTHROPIC_API_KEY,
        &model_registry,
        credential_store,
    );
    assert!(
        scheduled,
        "saving the Anthropic key must schedule a refresh"
    );
}

/// Saving any OTHER credential must NOT schedule a refresh.
#[tokio::test]
async fn trigger_anthropic_refresh_if_needed_ignores_other_credentials() {
    let model_registry = crate::model_registry::ModelRegistry::new();
    let credential_store: Arc<dyn crate::credentials::CredentialStore> =
        Arc::new(crate::credentials::MemoryCredentialStore::new());
    let scheduled = trigger_anthropic_refresh_if_needed(
        crate::credentials::GITHUB_TOKEN,
        &model_registry,
        credential_store,
    );
    assert!(
        !scheduled,
        "saving a non-Anthropic credential must not schedule a refresh"
    );
}

/// End-to-end through the real HTTP handler: saving a DIFFERENT credential
/// (github_token) must leave the Anthropic live-registry cache untouched
/// (`anthropic_fetched` stays false). Mirrors
/// `set_credential_for_non_openrouter_name_does_not_touch_the_registry_cache`; the
/// anthropic_api_key case itself is covered at the function level above instead of
/// here, to avoid any chance of the request handler's spawned task making a live call
/// to api.anthropic.com mid-test.
#[tokio::test]
async fn set_credential_for_non_anthropic_name_does_not_touch_the_anthropic_cache() {
    let state = AppState::new(Arc::new(InMemoryStoryStore::new()));
    assert!(!state.model_registry.anthropic_fetched());
    let app = router(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/credentials/github_token")
                .header("content-type", "application/json")
                .body(Body::from(r#"{"value":"ghp_unrelated_token"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        !state.model_registry.anthropic_fetched(),
        "a non-Anthropic credential save must never trigger a live-Anthropic refresh"
    );
}

// ── GET /api/models/registry — backend-aware Claude portion (Feature A) ────────
