#![allow(clippy::unwrap_used)]
//! END-TO-END (WO4): what stage 1 said about a site stays inside the engine. The scripted stage 1
//! reply carries a distinctive reason and lowers a critical finding to low, which records a
//! severity adjustment on the row. The real pipeline (`ai_audit::audit_repo`, the grouping, the
//! report, workbook, `findings.json`, CSV and JSON builders) then runs, and every client-visible
//! surface is read: the reason is on none of them, while the adjustment itself (the fixed
//! sentence naming the two severities) is on at least one, so the test is not vacuous.
//!
//! ZERO API SPEND: the stub never makes a network call.

use std::collections::HashMap;

use async_trait::async_trait;
use camerata_server::ai_audit::{self, ScanMode, UsageMeter};
use camerata_server::cost_cap::RunControls;
use camerata_server::efficiency::EfficiencyConfig;
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::{Finding, ScanReport};
use camerata_server::report_export::{self, ReportOptions};
use camerata_server::review_tree;
use camerata_server::scan_ledger::ScanLedger;
use camerata_server::xlsx_export;

const PATH: &str = "src/handlers/webhook.rs";
/// What stage 1 says about the site. Nothing else in the run contains it.
const REASON: &str = "ZEBRA-REASON-7731 the guard sits two calls up";

struct StubModel;

fn scan_response() -> String {
    format!(
        r#"{{"findings":[{{"path":"{PATH}","line":3,"severity":"critical","rule":"webhook-handler-skips-signature-check","title":"Webhook handler skips the signature check","code":"process_refund(payload.account_id);","detail":"The handler calls process_refund without verifying the caller."}}],"needs_files":[]}}"#
    )
}

#[async_trait]
impl LlmPort for StubModel {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let system = req.system.as_deref().unwrap_or_default();
        let text = if system.starts_with(&ai_audit::verify_system_prompt()) {
            format!(
                r#"{{"severity":"low","real":"yes","effort":"low","category":"authorization","reason":"{REASON}"}}"#
            )
        } else if system.starts_with("You are a senior software architect") {
            scan_response()
        } else {
            "{}".to_string()
        };
        Ok(LlmResponse {
            reported_model: None,
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
        repos: vec!["acme/webhooks".to_string()],
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

/// Run the pipeline and return every client-visible surface as text, with the merged findings.
async fn surfaces() -> (Vec<(String, String)>, Vec<Finding>) {
    let files = vec![(
        PATH.to_string(),
        "pub fn handle(payload: Payload) {\n    // no check\n    process_refund(payload.account_id);\n}\n".to_string(),
    )];
    let mut efficiency = EfficiencyConfig::default();
    efficiency.set("terse_output", false).unwrap();
    let meter = UsageMeter::with_controls(RunControls {
        stored_spend_usd: 0.0,
        review: Default::default(),
        efficiency,
        max_cost_usd: None,
        estimate_hint_usd: None,
        fix_model: None,
        clone_judge_model: None,
        consensus: false,
    });
    let (findings, _proposed, _recs, failed_passes, stage_samples) = ai_audit::audit_repo(
        &StubModel,
        "acme/webhooks",
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
        project_title: "Stage 1 reason E2E".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        // A stub run is not a complete run.
        internal: true,
        ..Default::default()
    };
    let mut scan = report(merged.clone(), ledger);
    scan.failed_passes = failed_passes;
    let sealed = scan.seal();
    let none = HashMap::new();

    let json = report_export::build_report_json(
        &camerata_server::report_export::ReviewedReport::unreviewed(&sealed, &none),
        Some(&corpus),
        &opts,
    );
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
    let export = xlsx_export::build_findings_export(
        &camerata_server::report_export::ReviewedReport::unreviewed(&sealed, &none),
        Some(&corpus),
        &json,
        &HashMap::new(),
    );
    out.push((
        "findings.json".to_string(),
        serde_json::to_string_pretty(&export).unwrap(),
    ));
    for (name, bytes) in xlsx_export::build_csv_exports(
        &camerata_server::report_export::ReviewedReport::unreviewed(&sealed, &none),
        Some(&corpus),
        &opts,
    )
    .unwrap()
    .into_iter()
    .chain(
        xlsx_export::build_json_exports(
            &camerata_server::report_export::ReviewedReport::unreviewed(&sealed, &none),
            Some(&corpus),
            &opts,
        )
        .unwrap(),
    ) {
        out.push((name, String::from_utf8_lossy(&bytes).into_owned()));
    }
    let workbook = xlsx_export::build_workbook(
        &camerata_server::report_export::ReviewedReport::unreviewed(&sealed, &none),
        Some(&corpus),
        &opts,
    )
    .unwrap();
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(workbook)).unwrap();
    for i in 0..archive.len() {
        let mut part = archive.by_index(i).unwrap();
        let mut text = String::new();
        std::io::Read::read_to_string(&mut part, &mut text).ok();
        out.push((format!("workbook {}", part.name().unwrap()), text));
    }
    (out, merged)
}

#[tokio::test]
async fn no_client_facing_output_carries_the_stage_1_reason() {
    let (surfaces, merged) = surfaces().await;
    // Anti-vacuity: the finding is there, stage 1 lowered it, and the reason is kept on the row.
    assert_eq!(merged.len(), 1, "{merged:#?}");
    assert_eq!(merged[0].severity, "low", "stage 1 lowered the severity");
    let kept = merged[0]
        .captures
        .get(ai_audit::STAGE1_CAPTURE)
        .expect("the stage 1 verdict is kept on the row");
    assert!(kept.contains("ZEBRA-REASON-7731"), "{kept}");
    for name in ["PDF data", "report json", "findings.json"] {
        assert!(
            surfaces.iter().any(|(n, _)| n == name),
            "{name} was not built"
        );
    }
    assert!(
        surfaces
            .iter()
            .any(|(_, text)| text.contains(PATH) && text.to_lowercase().contains("lowered")),
        "the severity adjustment itself is on a client surface, so the test reads real output"
    );
    for (name, text) in &surfaces {
        assert!(
            !text.contains("ZEBRA-REASON-7731") && !text.contains("guard sits two calls up"),
            "the stage 1 reason reached {name}"
        );
    }
}
