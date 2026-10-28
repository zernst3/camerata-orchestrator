#![allow(clippy::unwrap_used)]
//! Runtime end-of-life and runtime vulnerability findings, from the declared versions in a
//! repository folder to the rendered report: the PDF's data (and the PDF itself when Typst is on
//! the path), the workbook, the README stamp lines and the Coverage disclosure.
//!
//! Every feed answer is a recorded fixture served through `FeedFetcher`; nothing here touches the
//! network. The rules come from the REAL shipped corpus, so a finding that names a rule the
//! corpus does not carry (or carries without a title or a fix) fails here.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Mutex;

use camerata_server::onboard::{
    AuditedRef, Finding, LanguageVolume, ScanProvenance, ScanReport, SealedReport,
};
use camerata_server::report_export::{build_report_json, render_data_json, ReportOptions};
use camerata_server::runtime_audit::{
    audit_runtimes, NvdPolicy, RuntimeAudit, EOL_RULE_ID, VULN_RULE_ID,
};
use camerata_server::vuln_intel::{
    enrich_with, FeedFetcher, OsvRun, VulnDataStamp, EPSS_URL, KEV_URL,
};
use chrono::{TimeZone, Utc};

const EOL_NODE: &str = include_str!("fixtures/runtime_audit/endoflife_nodejs.json");
const EOL_PYTHON: &str = include_str!("fixtures/runtime_audit/endoflife_python.json");
const EOL_DOTNET: &str = include_str!("fixtures/runtime_audit/endoflife_dotnet.json");
const NVD_NODE: &str = include_str!("fixtures/runtime_audit/nvd_nodejs_18.17.0.json");
const KEV_FIXTURE: &str = include_str!("fixtures/vuln_intel/kev.json");
const EPSS: &str = r#"{"status":"OK","data":[{"cve":"CVE-2023-44487","epss":"0.944","percentile":"0.999","date":"2026-10-27"}]}"#;

/// Serves the recorded feeds. A feed named in `down` fails like an unreachable host.
struct Feeds {
    down: Vec<&'static str>,
    calls: Mutex<Vec<String>>,
}

impl Feeds {
    fn new(down: &[&'static str]) -> Self {
        Self {
            down: down.to_vec(),
            calls: Mutex::new(Vec::new()),
        }
    }
}

fn kev() -> String {
    let mut doc: serde_json::Value = serde_json::from_str(KEV_FIXTURE).unwrap();
    let rows = doc["vulnerabilities"].as_array_mut().unwrap();
    rows.push(serde_json::json!({"cveID": "CVE-2023-44487", "dateAdded": "2023-10-10"}));
    for i in 0..camerata_server::vuln_intel::KEV_MIN_ENTRIES {
        rows.push(
            serde_json::json!({"cveID": format!("CVE-1999-{i:05}"), "dateAdded": "2000-01-01"}),
        );
    }
    serde_json::to_string(&doc).unwrap()
}

#[async_trait::async_trait]
impl FeedFetcher for Feeds {
    async fn get(&self, url: &str) -> anyhow::Result<String> {
        self.calls.lock().unwrap().push(url.to_string());
        let (feed, body) = if url.starts_with("https://endoflife.date/api/nodejs") {
            ("endoflife", EOL_NODE.to_string())
        } else if url.starts_with("https://endoflife.date/api/python") {
            ("endoflife", EOL_PYTHON.to_string())
        } else if url.starts_with("https://endoflife.date/api/dotnet") {
            ("endoflife", EOL_DOTNET.to_string())
        } else if url.contains("services.nvd.nist.gov") && url.contains("node.js:18.17.0") {
            ("nvd", NVD_NODE.to_string())
        } else if url == KEV_URL {
            ("kev", kev())
        } else if url.starts_with(EPSS_URL) {
            ("epss", EPSS.to_string())
        } else {
            anyhow::bail!("no recorded answer for {url}")
        };
        if self.down.contains(&feed) {
            anyhow::bail!("{feed} unreachable");
        }
        Ok(body)
    }
}

fn now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap()
}

/// A repository declaring: Node 18.17.0 (past end of life, with known vulnerabilities, one of
/// them known exploited), Python 3.8 (past end of life) and .NET 8 (ends in 13 days).
fn declared_repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        ("Dockerfile", "FROM node:18.17.0-alpine\n"),
        (".python-version", "3.8\n"),
        (
            "src/App.csproj",
            "<Project>\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n  </PropertyGroup>\n</Project>\n",
        ),
    ];
    for (path, body) in files {
        let full = dir.path().join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    dir
}

async fn audit(dir: &tempfile::TempDir, feeds: &Feeds) -> RuntimeAudit {
    audit_runtimes(
        &[("acme/app".to_string(), dir.path().to_path_buf())],
        feeds,
        None,
        now(),
        &NvdPolicy::instant(),
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(5),
    )
    .await
}

/// The sealed report the scan pipeline would hand the exporters: the runtime rows after KEV and
/// EPSS enrichment, the pass's Coverage notes and both feeds' stamps.
async fn sealed_report(feeds: &Feeds) -> SealedReport {
    let dir = declared_repo();
    let runtime = audit(&dir, feeds).await;
    let mut findings: Vec<Finding> = runtime.findings.clone();
    let osv = vec![OsvRun {
        repo: "acme/app".to_string(),
        ran: true,
    }];
    let (mut stamp, notes) = enrich_with(&mut findings, &osv, feeds, None, now()).await;
    stamp.endoflife = runtime.eol.clone();
    stamp.nvd = runtime.nvd.clone();
    let mut coverage_notes = runtime.notes.clone();
    coverage_notes.extend(notes);
    ScanReport {
        repos: vec!["acme/app".to_string()],
        stacks: Vec::new(),
        files_scanned: 10,
        test_file_count: 0,
        files_excluded: 0,
        excluded_noise_lines: None,
        code_chars: 5000,
        code_lines: 400,
        code_lines_by_language: vec![LanguageVolume {
            language: "JavaScript".to_string(),
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
        coverage_notes,
        provenance: ScanProvenance {
            audited_refs: vec![AuditedRef {
                repo: "acme/app".to_string(),
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
            audited_rule_ids: Vec::new(),
            camerata_version: "0.0.0-test".to_string(),
            osv_scanner_version: None,
            vulnerability_data: Some(stamp),
            started_at: "2026-10-28T00:00:00Z".to_string(),
            finished_at: "2026-10-28T00:05:00Z".to_string(),
            model_sessions: Vec::new(),
        },
        recommendations: HashMap::new(),
        failed_passes: Vec::new(),
        ledger: camerata_server::scan_ledger::ScanLedger::new(),
    }
    .seal()
}

async fn real_corpus() -> Option<camerata_rules::RuleSet> {
    let path = std::path::Path::new(camerata_rules::DEFAULT_CORPUS_PATH);
    if !path.exists() {
        return None;
    }
    Some(camerata_rules::load_corpus(path).await.unwrap())
}

fn options() -> ReportOptions {
    ReportOptions {
        client_name: "Example Client Ltd".to_string(),
        ..ReportOptions::default()
    }
}

/// Text with every run of whitespace removed, so a sentence the PDF wrapped still matches.
fn squeezed(text: &str) -> String {
    text.split_whitespace().collect()
}

fn xlsx_text(bytes: &[u8]) -> String {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut xml = String::new();
    zip.by_name("xl/sharedStrings.xml")
        .unwrap()
        .read_to_string(&mut xml)
        .unwrap();
    xml.replace("&amp;", "&")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[tokio::test]
async fn the_findings_reach_the_pdf_data_the_workbook_and_the_findings_json() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    let sealed = sealed_report(&Feeds::new(&[])).await;
    let report_json = build_report_json(&sealed, &HashMap::new(), Some(&corpus), &options());

    // Both rules exist in the shipped corpus with a title, sources and a fix.
    for id in [EOL_RULE_ID, VULN_RULE_ID] {
        let rule = corpus
            .get_by_id(id)
            .unwrap_or_else(|| panic!("{id} is in the corpus"));
        assert!(!rule.title.is_empty());
        assert!(!rule.sources.is_empty(), "{id} cites its sources");
        assert!(!rule.is_inspectable(), "{id} is emit-only");
    }

    // Every runtime finding is in the report, curated or held, none dropped.
    let sites: Vec<_> = report_json
        .curated_findings
        .iter()
        .chain(report_json.held_for_review_findings.iter())
        .flat_map(|g| g.sites.iter().map(move |s| (g, s)))
        .collect();
    let rows = sealed.findings.len();
    assert_eq!(
        rows, 4,
        "node EOL, node CVEs, python EOL, dotnet advisory: {:?}",
        sealed.findings
    );
    assert_eq!(
        sites.len(),
        rows,
        "no runtime row is dropped from the report"
    );

    let data = render_data_json(&report_json).unwrap();
    let data = squeezed(&data);
    for sentence in [
        "Node.js 18 has received no security fixes since 2025-04-30.",
        "Python 3.8 has received no security fixes since 2024-10-07.",
        "Advisory: .NET 8 reaches its end of life on 2026-11-10, in 13 days",
        "Node.js 18.17.0 has 4 known vulnerabilities recorded in the National Vulnerability Database.",
        "Known exploited: CVE-2023-44487 is listed in the CISA KEV catalog (added 2023-10-10). Severity raised from high to critical.",
    ] {
        assert!(data.contains(&squeezed(sentence)), "the PDF data lacks: {sentence}");
    }
    // The rule's own authored remediation reaches the report.
    assert!(
        data.contains(&squeezed("release line that still receives security fixes")),
        "the corpus remediation is rendered"
    );

    // Severity: end of life is high, the advisory low, the known-exploited runtime critical.
    let severity_of = |needle: &str| {
        sealed
            .findings
            .iter()
            .find(|f| f.detail.contains(needle))
            .map(|f| f.severity.clone())
            .unwrap_or_else(|| panic!("no finding with {needle}"))
    };
    assert_eq!(severity_of("Node.js 18 has received"), "high");
    assert_eq!(severity_of("Python 3.8 has received"), "high");
    assert_eq!(severity_of("Advisory: .NET 8"), "low");
    assert_eq!(severity_of("Node.js 18.17.0 has 4"), "critical");

    // The workbook.
    let xlsx = camerata_server::xlsx_export::build_workbook(
        &sealed,
        &HashMap::new(),
        Some(&corpus),
        &options(),
    )
    .unwrap();
    let text = xlsx_text(&xlsx);
    assert!(text.contains("Node.js 18 has received no security fixes since 2025-04-30."));
    assert!(text.contains("Node.js 18.17.0 has 4 known vulnerabilities"));

    // findings.json.
    let export = camerata_server::xlsx_export::build_findings_export(
        &sealed,
        &HashMap::new(),
        Some(&corpus),
        &report_json,
        &HashMap::new(),
    );
    let json = serde_json::to_string(&export).unwrap();
    assert!(json.contains("SEC-NO-END-OF-LIFE-RUNTIME-1"));
    assert!(json.contains("SEC-NO-KNOWN-VULNERABLE-RUNTIME-1"));

    // The runtime rules never read "verified clean" in "what's healthy".
    assert!(
        !squeezed(&serde_json::to_string(&report_json.whats_healthy).unwrap()).contains("RUNTIME")
    );
}

#[tokio::test]
async fn a_feed_that_could_not_be_fetched_is_disclosed_and_never_reads_as_clean() {
    let Some(corpus) = real_corpus().await else {
        return;
    };
    // endoflife.date and the NVD are both down.
    let sealed = sealed_report(&Feeds::new(&["endoflife", "nvd"])).await;
    assert!(sealed.findings.is_empty(), "{:?}", sealed.findings);
    let report_json = build_report_json(&sealed, &HashMap::new(), Some(&corpus), &options());
    let data = squeezed(&render_data_json(&report_json).unwrap());
    assert!(
        data.contains(&squeezed(
            "Runtime end-of-life data unavailable in this scan"
        )),
        "the Coverage disclosure is in the report"
    );
    assert!(
        !data.contains("RUNTIME-1"),
        "no runtime rule reads as checked and clean"
    );

    // The stamp says not checked, in the README line and the manifest line alike.
    let stamp: &VulnDataStamp = sealed.provenance.vulnerability_data.as_ref().unwrap();
    let lines = camerata_server::vuln_intel::readme_lines(Some(stamp));
    assert!(
        lines.contains("Vulnerability data (End-of-life, endoflife.date) not checked ("),
        "{lines}"
    );
    assert!(
        lines.contains("Vulnerability data (NVD, runtimes) not checked ("),
        "{lines}"
    );
}

#[tokio::test]
async fn the_rendered_pdf_and_readme_carry_the_findings_and_the_stamp() {
    if !camerata_test_env::typst_on_path() {
        eprintln!(
            "skipping the_rendered_pdf_and_readme_carry_the_findings_and_the_stamp: no typst"
        );
        return;
    }
    let Some(corpus) = real_corpus().await else {
        return;
    };
    let sealed = sealed_report(&Feeds::new(&[])).await;
    let package = camerata_server::release_gate::build_product_package(
        &sealed,
        &HashMap::new(),
        Some(&corpus),
        &options(),
    )
    .await
    .expect("the package assembles");
    let text = pdf_extract::extract_text_from_mem(&package.pdf).expect("the PDF's text extracts");
    let flat = squeezed(&text);
    for sentence in [
        "Node.js 18 has received no security fixes since 2025-04-30.",
        "Node.js 18.17.0 has 4 known vulnerabilities",
    ] {
        assert!(
            flat.contains(&squeezed(sentence)),
            "the rendered PDF lacks: {sentence}"
        );
    }
    assert!(package
        .readme
        .contains("Vulnerability data (End-of-life, endoflife.date) as of 2026-10-28"));
    assert!(package
        .readme
        .contains("Vulnerability data (NVD, runtimes) as of 2026-10-28"));
    // The release gate holds the README and the manifest together; a package with the new stamp
    // lines still clears it.
    camerata_server::release_gate::release(package).expect("the package clears the release gate");
}
