#![allow(clippy::unwrap_used)]
//! END-TO-END (defect D5): a credential on a line the scan reported under two rules is redacted
//! everywhere the merged finding reaches, with `terse_output` on and off.
//!
//! The scan stub reports the SAME line twice under different rule names, so the real merge folds
//! one report into the other and the absorbed site lands in `also_locations`. The real production
//! functions then run (`ai_audit::audit_repo` -> `ai_audit::group_semantic` -> the report,
//! workbook, `findings.json` and dataset builders), and every client-visible surface is read: no
//! surface may hold the key, and the merged row must really be there (anti-vacuity).
//!
//! ZERO API SPEND: the stub never makes a network call.

use std::collections::HashMap;

use async_trait::async_trait;
use camerata_secrets::fixtures::{planted, Planted};
use camerata_secrets::SecretShape;
use camerata_server::ai_audit::{self, ScanMode, UsageMeter};
use camerata_server::cost_cap::RunControls;
use camerata_server::efficiency::EfficiencyConfig;
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::{Finding, ScanReport};
use camerata_server::report_export::{self, ReportOptions};
use camerata_server::review_tree;
use camerata_server::scan_ledger::ScanLedger;
use camerata_server::xlsx_export;

const PATH: &str = "src/config.ts";

struct StubModel {
    code_line: String,
}

fn scan_response(code_line: &str) -> String {
    let code = serde_json::to_string(code_line).unwrap();
    format!(
        r#"{{"findings":[
            {{"path":"{PATH}","line":2,"severity":"critical","rule":"hardcoded-payment-key-in-source","title":"Payment secret key committed to source","code":{code},"detail":"The payment secret key is written into the source file."}},
            {{"path":"{PATH}","line":2,"severity":"high","rule":"secret-key-shipped-with-client-bundle","title":"Secret key ships in the client bundle","code":{code},"detail":"The same key is bundled with the client."}}
        ],"needs_files":[]}}"#
    )
}

#[async_trait]
impl LlmPort for StubModel {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let system = req.system.as_deref().unwrap_or_default();
        let text = if system.starts_with("You are a senior software architect") {
            scan_response(&self.code_line)
        } else {
            // Detail, calibration and anything else: an unparseable reply passes findings through.
            "{}".to_string()
        };
        Ok(LlmResponse {
            text,
            model: "stub".to_string(),
            backend: "stub".to_string(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
            truncated: false,
        })
    }

    async fn complete_streaming(
        &self,
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let response = self.complete(req).await?;
        on_delta(&response.text);
        Ok(response)
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn report(findings: Vec<Finding>, ledger: ScanLedger) -> ScanReport {
    ScanReport {
        run_id: None,
        tree: None,
        discarded: Vec::new(),
        repos_not_inspected: Vec::new(),
        recommendations_by_repo: Vec::new(),
        patterns_across_services: Default::default(),
        volume_by_repo: Default::default(),
        repos: vec!["acme/shop".to_string()],
        stacks: Vec::new(),
        files_scanned: 1,
        test_file_count: 0,
        files_excluded: 0,
        excluded_noise_lines: None,
        code_chars: 200,
        code_lines: 4,
        code_lines_by_language: Vec::new(),
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
        provenance: Default::default(),
        recommendations: HashMap::new(),
        failed_passes: Vec::new(),
        ledger,
    }
}

fn leaked(haystack: &str, p: &Planted) -> bool {
    p.value.lines().any(|line| haystack.contains(line))
}

/// Run the pipeline and return every client-visible surface as text, with the merged findings.
async fn surfaces(terse_output: bool, p: &Planted) -> (Vec<(String, String)>, Vec<Finding>) {
    let code_line = format!("const stripeKey = \"{}\";", p.value);
    let files = vec![(
        PATH.to_string(),
        format!("import Stripe from \"stripe\";\n{code_line}\nexport const stripe = new Stripe(stripeKey);\n"),
    )];
    let stub = StubModel { code_line };
    let mut efficiency = EfficiencyConfig::default();
    efficiency.set("terse_output", terse_output).unwrap();
    let meter = UsageMeter::with_controls(RunControls {
        review: Default::default(),
        efficiency,
        max_cost_usd: None,
        estimate_hint_usd: None,
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    });
    let (findings, _proposed, _recs, failed_passes, stage_samples) = ai_audit::audit_repo(
        &stub,
        "acme/shop",
        &files,
        &[],
        &[],
        &HashMap::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        None,
        None,
        Some(&meter),
        Some(&files),
    )
    .await
    .expect("audit_repo against the stub");
    let merged = review_tree::collapse_and_group(findings, &files);

    let mut ledger = ScanLedger::new();
    for sample in stage_samples {
        ledger.record_stage_sample(sample);
    }
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "{errors:?}");
    let opts = ReportOptions {
        client_name: "Acme Corp".to_string(),
        project_title: "Merged site redaction E2E".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        // A stub run is not a complete run.
        internal: true,
        ..Default::default()
    };
    let mut scan = report(merged.clone(), ledger);
    scan.failed_passes = failed_passes;
    let sealed = scan.seal();
    let none = HashMap::new();

    let json = report_export::build_report_json(&sealed, &none, Some(&corpus), &opts);
    let mut out = vec![
        (
            "PDF data".to_string(),
            report_export::render_data_json(&json).expect("data.json renders"),
        ),
        (
            "report json".to_string(),
            serde_json::to_string(&json).unwrap(),
        ),
    ];
    let export =
        xlsx_export::build_findings_export(&sealed, &none, Some(&corpus), &json, &HashMap::new());
    out.push((
        "findings.json".to_string(),
        serde_json::to_string_pretty(&export).unwrap(),
    ));
    for (name, bytes) in xlsx_export::build_csv_exports(&sealed, &none, Some(&corpus), &opts)
        .unwrap()
        .into_iter()
        .chain(xlsx_export::build_json_exports(&sealed, &none, Some(&corpus), &opts).unwrap())
    {
        out.push((name, String::from_utf8_lossy(&bytes).into_owned()));
    }
    // The workbook is a zip of XML parts; the key (alphanumeric) would survive XML escaping, so
    // the decompressed parts are searched as text.
    let workbook = xlsx_export::build_workbook(&sealed, &none, Some(&corpus), &opts).unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(workbook)).unwrap();
    for i in 0..archive.len() {
        let mut part = archive.by_index(i).unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut part, &mut text).ok();
        out.push((format!("workbook {}", part.name()), text));
    }
    (out, merged)
}

async fn assert_redacted_everywhere(terse_output: bool) {
    for shape in [
        SecretShape::StripeLiveSecretKey,
        SecretShape::SupabaseSecretKey,
    ] {
        let p = planted(shape);
        let (surfaces, merged) = surfaces(terse_output, &p).await;
        // Anti-vacuity: the two reports of the same key (under two rule names) are two rows now,
        // each carrying the credential's place.
        assert!(
            merged.len() >= 2,
            "terse_output={terse_output}: the fixture must produce two rows: {merged:#?}"
        );
        assert!(
            surfaces.iter().any(|(n, _)| n == "findings.json"),
            "findings.json was not built"
        );
        for (name, text) in &surfaces {
            assert!(
                !leaked(text, &p),
                "terse_output={terse_output} {shape:?}: the key reached {name}"
            );
        }
    }
}

#[tokio::test]
async fn a_merged_credential_site_is_redacted_everywhere_with_terse_output_on() {
    assert_redacted_everywhere(true).await;
}

#[tokio::test]
async fn a_merged_credential_site_is_redacted_everywhere_with_terse_output_off() {
    assert_redacted_everywhere(false).await;
}
