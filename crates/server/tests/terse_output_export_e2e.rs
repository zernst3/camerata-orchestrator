//! END-TO-END (rank 4, `terse_output`): a short scan `detail` never reaches the client export.
//!
//! The scan stub answers in the terse format (a one-sentence `detail`, no `proposed_rules`). The
//! real production functions a scan calls after the model responds then run
//! (`ai_audit::audit_repo` -> `ai_audit::merge_semantic_groups` ->
//! `report_export::build_report_json`), and the test reads the EXPORT:
//!
//! * regeneration works: every exported finding carries the regenerated full detail, and the
//!   one-sentence statement the scan wrote is nowhere in the export;
//! * regeneration fails: the finding is still in the export, with the incomplete marker beside
//!   its short detail, and the export discloses the failed pass. Never dropped, never silently
//!   short.
//!
//! ZERO API SPEND: the stub never makes a network call.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use camerata_server::ai_audit::{self, FailedPass, ScanMode, UsageMeter};
use camerata_server::cost_cap::RunControls;
use camerata_server::efficiency::EfficiencyConfig;
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::ScanReport;
use camerata_server::report_export::{self, ReportOptions};
use camerata_server::scan_ledger::ScanLedger;

const HANDLER_PATH: &str = "src/handlers/webhook.rs";
const HANDLER_SNIPPET: &str = "process_refund(payload.account_id, payload.amount_cents);";
/// What the terse scan writes.
const SHORT: &str = "Trusts REQUIRE_WEBHOOK_SIGNATURE from src/config.rs.";
/// What the regeneration writes.
const FULL: &str = "The handler reads REQUIRE_WEBHOOK_SIGNATURE from src/config.rs and, when it is false, calls process_refund with no signature check, so a forged webhook request triggers a real refund.";
const MARKER: &str = "[detail incomplete]";

/// Answers the scan with a terse-format finding, and the regeneration with [`FULL`] or, when
/// `regeneration_down` is set, with a transport error.
struct StubModel {
    regeneration_down: AtomicBool,
}

fn scan_response() -> String {
    format!(
        r#"{{"findings":[{{"path":"{HANDLER_PATH}","line":9,"severity":"critical","rule":"config-flag-disables-signature-check-before-privileged-refund","title":"Webhook handler trusts a config flag and skips signature verification before issuing a refund","code":"{HANDLER_SNIPPET}","detail":"{SHORT}"}}],"needs_files":[]}}"#
    )
}

#[async_trait]
impl LlmPort for StubModel {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        let system = req.system.as_deref().unwrap_or_default();
        let text = if system.starts_with("You write the full explanation") {
            if self.regeneration_down.load(Ordering::SeqCst) {
                anyhow::bail!("transport down");
            }
            // Address the reply the way the prompt asks: the finding's id, path and line.
            let header = req
                .prompt
                .lines()
                .find(|l| l.starts_with('[') && l.contains("] rule "))
                .expect("the regeneration prompt addresses the finding by an id");
            let id = &header[1..header.find(']').expect("a closing bracket")];
            let line = header.rsplit(':').next().expect("a line").trim();
            format!(
                r#"{{"details":[{{"id":"{id}","path":"{HANDLER_PATH}","line":{line},"detail":"{FULL}"}}]}}"#
            )
        } else if system.starts_with("You are a senior software architect") {
            scan_response()
        } else {
            // Calibration and anything else: an unparseable reply passes findings through.
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

fn fixture_files() -> Vec<(String, String)> {
    vec![
        (
            "src/config.rs".to_string(),
            "pub const REQUIRE_WEBHOOK_SIGNATURE: bool = false;\n".to_string(),
        ),
        (
            HANDLER_PATH.to_string(),
            "use crate::config::REQUIRE_WEBHOOK_SIGNATURE;\n\
             \n\
             pub fn handle_webhook(payload: WebhookPayload) {\n\
             \x20   if REQUIRE_WEBHOOK_SIGNATURE {\n\
             \x20       verify_hmac_signature(&payload);\n\
             \x20   }\n\
             \x20   // No signature check ran when the flag above is false.\n\
             \x20   // Nothing else verifies the caller's identity before this call.\n\
             \x20   process_refund(payload.account_id, payload.amount_cents);\n\
             }\n"
            .to_string(),
        ),
    ]
}

fn report(
    findings: Vec<camerata_server::onboard::Finding>,
    ledger: ScanLedger,
    failed_passes: Vec<FailedPass>,
) -> ScanReport {
    ScanReport {
        repos: vec!["acme/webhooks".to_string()],
        stacks: Vec::new(),
        files_scanned: 2,
        test_file_count: 0,
        files_excluded: 0,
        excluded_noise_lines: None,
        code_chars: 400,
        code_lines: 15,
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
        failed_passes,
        ledger,
    }
}

/// Run the real pipeline with the stub and return the export serialized to JSON text, plus the
/// findings `audit_repo` returned.
async fn export_text(regeneration_down: bool) -> (String, Vec<camerata_server::onboard::Finding>) {
    let files = fixture_files();
    let stub = StubModel {
        regeneration_down: AtomicBool::new(regeneration_down),
    };
    // The default switches: `terse_output` is on.
    let meter = UsageMeter::with_controls(RunControls {
        efficiency: EfficiencyConfig::default(),
        max_cost_usd: None,
        estimate_hint_usd: None,
        fix_model: None,
        consensus: false,
    });
    let (findings, _proposed, _recs, failed_passes, stage_samples) = ai_audit::audit_repo(
        &stub,
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
    let audited = findings.clone();
    let merged = ai_audit::merge_semantic_groups(findings, &files);

    let mut ledger = ScanLedger::new();
    for sample in stage_samples {
        ledger.record_stage_sample(sample);
    }
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(errors.is_empty(), "{errors:?}");
    let opts = ReportOptions {
        client_name: "Acme Corp".to_string(),
        project_title: "Terse output export E2E".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        ..Default::default()
    };
    let json = report_export::build_report_json(
        &report(merged, ledger, failed_passes).seal(),
        &HashMap::new(),
        Some(&corpus),
        &opts,
    );
    (
        serde_json::to_string(&json).expect("the export serializes"),
        audited,
    )
}

#[tokio::test]
async fn every_exported_finding_carries_the_regenerated_full_detail() {
    let (export, audited) = export_text(false).await;

    assert_eq!(audited.len(), 1);
    assert!(audited[0].detail.contains(FULL), "{}", audited[0].detail);
    assert!(
        export.contains(FULL),
        "the export must carry the regenerated full detail"
    );
    assert!(
        !export.contains(SHORT),
        "the one-sentence statement the scan wrote must not reach the export"
    );
    assert!(
        !export.contains(MARKER),
        "nothing failed, nothing is marked"
    );
    assert!(
        !export.contains("Detail regeneration"),
        "no failed pass is disclosed on the happy path"
    );
}

#[tokio::test]
async fn a_regeneration_failure_keeps_the_finding_in_the_export_marked_and_disclosed() {
    let (export, audited) = export_text(true).await;

    assert_eq!(audited.len(), 1, "the finding is not dropped");
    assert!(audited[0].detail.contains(SHORT));
    assert!(audited[0].detail.contains(MARKER));
    // Still in the export, with the short detail ONLY beside the marker.
    assert!(
        export.contains(HANDLER_PATH),
        "the finding must still be in the export"
    );
    assert!(
        export.contains(SHORT),
        "the scan's sentence is what is left"
    );
    assert!(
        export.contains(MARKER),
        "a short detail never ships without the incomplete marker"
    );
    assert!(
        !export.contains(FULL),
        "no full detail was written, none can be exported"
    );
    // The existing failed-pass disclosure names the step.
    assert!(
        export.contains("Detail regeneration for acme/webhooks: not computed"),
        "the export discloses the failed pass: {export}"
    );
}
