//! END-TO-END (W2): the semantic/AI tier's CROSS-FILE BACKSTOP role — the one layer in the
//! pipeline that can reason across files at all (the external taint tools Camerata shells out
//! to are file-at-a-time), and the one layer that can report a security-relevant observation
//! for which NO rule exists.
//!
//! Motivation: on a real unseen repo the semantic tier once emitted ZERO rows while five real
//! security defects went undetected — a silent collapse indistinguishable from "nothing to
//! report." This test drives the REAL production functions a scan calls after the model
//! responds (`ai_audit::audit_repo` -> `ai_audit::merge_semantic_groups` ->
//! `report_export::build_report_json`) with a STUBBED `LlmPort` standing in for the model, over
//! a synthetic two-file fixture with a classic cross-file config-to-handler defect: a
//! verification flag defined in one file, and a privileged handler in ANOTHER file that trusts
//! it with no compensating check. No rule in the (empty) selected set covers this — it can only
//! ever surface via the semantic tier's advisory/backstop pass — and it must still reach the
//! export as a visible, honestly-labeled row, never silently dropped.
//!
//! Why not drive this through `onboard::audit_repos` like the other `*_executor_e2e.rs` suites:
//! that entry point always constructs a REAL network-backed `Llm` from the process environment
//! with no `LlmPort` injection seam, so a zero-spend stubbed-model e2e test has to assemble the
//! same real pipeline functions `audit_repos` itself calls, rather than go through it. Every
//! function this test calls (`audit_repo`, `merge_semantic_groups`, `build_report_json`) is the
//! exact, unmodified production code path — only the network call is replaced.
//!
//! ZERO API SPEND: the stub never makes a network call.

use std::collections::HashMap;

use async_trait::async_trait;
use camerata_server::ai_audit::{self, ScanMode};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::ScanReport;
use camerata_server::report_export::{self, ReportOptions};
use camerata_server::scan_ledger::ScanLedger;

/// The handler file + the exact offending line the stub's canned finding cites — named so the
/// assertions below read as "does the REAL line survive," not a magic number.
const HANDLER_PATH: &str = "src/handlers/webhook.rs";
const HANDLER_SNIPPET: &str = "process_refund(payload.account_id, payload.amount_cents);";

/// A fake model that always answers with ONE canned finding naming a cross-file config+handler
/// defect — standing in for a real call to `audit_system_prompt()`'s JSON contract. The finding
/// deliberately uses a bare kebab-case `rule` (no adopted corpus id matches it, since the test
/// passes an EMPTY selected-rule set) so `ai_audit::parse_ai_findings` tags it `AI-`-prefixed:
/// exactly the "no deterministic sibling exists" shape this test is about. The SAME canned text
/// is returned to every call (the calibration pass's `verdicts` lookup fails to parse it and
/// falls back to passthrough, which is itself a `verify_findings`-documented safe default, not a
/// test artifact this fixture relies on).
struct StubModel;

const CANNED_FINDING_JSON: &str = r#"{
  "findings": [
    {
      "path": "src/handlers/webhook.rs",
      "line": 9,
      "severity": "critical",
      "rule": "config-flag-disables-signature-check-before-privileged-refund",
      "title": "Webhook handler trusts a config flag and skips signature verification before issuing a refund",
      "code": "process_refund(payload.account_id, payload.amount_cents);",
      "detail": "src/config.rs defines REQUIRE_WEBHOOK_SIGNATURE = false; this handler in src/handlers/webhook.rs reads that same flag and, when it is false, calls process_refund() directly with no HMAC signature check and no other compensating control on the caller's identity — a forged webhook request triggers a real refund.",
      "captures": {}
    }
  ],
  "proposed_rules": [],
  "needs_files": []
}"#;

#[async_trait]
impl LlmPort for StubModel {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(LlmResponse {
            text: CANNED_FINDING_JSON.to_string(),
            model: "stub".to_string(),
            backend: "stub".to_string(),
            cost_usd: None,
            input_tokens: None,
            output_tokens: None,
            cache_read_input_tokens: 0,
            cache_creation_input_tokens: 0,
            or_cache_discount: None,
        })
    }

    async fn complete_streaming(
        &self,
        _req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        on_delta(CANNED_FINDING_JSON);
        self.complete(LlmRequest::new("")).await
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
             \x20   // No signature check ran when the flag above is false — nothing else\n\
             \x20   // verifies the caller's identity before this privileged call.\n\
             \x20   process_refund(payload.account_id, payload.amount_cents);\n\
             }\n"
            .to_string(),
        ),
    ]
}

fn minimal_report(
    findings: Vec<camerata_server::onboard::Finding>,
    ledger: ScanLedger,
) -> ScanReport {
    ScanReport {
        repos: vec!["acme/webhooks".to_string()],
        stacks: Vec::new(),
        files_scanned: 2,
        test_file_count: 0,
        files_excluded: 0,
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
        failed_passes: Vec::new(),
        ledger,
    }
}

#[tokio::test]
async fn cross_file_config_handler_defect_survives_the_semantic_tier_to_the_export() {
    let files = fixture_files();
    let stub = StubModel;

    // No adopted rule covers this defect class — the EMPTY selected set is the whole point:
    // the only way this finding can ever surface is through the semantic tier's advisory/
    // backstop pass (`audit_system_prompt`'s "ALSO flag any other genuine issues" instruction),
    // never a corpus rule match.
    let selected: Vec<(String, String)> = Vec::new();

    let (findings, _proposed, _recs, failed_passes, stage_samples) = ai_audit::audit_repo(
        &stub,
        "acme/webhooks",
        &files,
        &selected,
        &[], // alternatives
        &HashMap::new(),
        None, // model
        None, // calibration_model
        ScanMode::Sequential,
        false, // thorough
        None,  // feedback
        None,  // job
        None,  // meter
        Some(&files),
    )
    .await
    .expect("audit_repo must succeed against the stub model");

    // The stub returns the SAME findings-shaped JSON to every call, including the calibration
    // round-trip — which expects a `{"verdicts": [...]}` shape and therefore cannot parse it.
    // `verify_findings`/`generate_fix_specifics` are documented fail-soft (never drop a finding
    // on an unparseable response — see `apply_verdicts`'s doc comment), so this is an EXPECTED,
    // disclosed degradation of this fixture, not a pipeline defect: assert it is the ONLY
    // disclosure, never a silent swallow.
    for fp in &failed_passes {
        assert!(
            fp.pass.contains("estimation") || fp.pass.contains("calibrat"),
            "only the calibration/estimation round-trip is expected to fail against this \
             fixture's single canned response shape: {failed_passes:?}"
        );
    }

    // ═══ The model's cross-file finding actually came through, tagged AI-tier. ═══
    assert_eq!(
        findings.len(),
        1,
        "exactly one finding, no deterministic sibling exists for it: {findings:?}"
    );
    let f = &findings[0];
    assert!(
        f.rule_id.starts_with("AI-"),
        "an id with no adopted-rule match must be AI-tier: {}",
        f.rule_id
    );
    assert_eq!(
        f.path, HANDLER_PATH,
        "the finding must anchor to the real offending file"
    );
    assert_eq!(
        f.snippet, HANDLER_SNIPPET,
        "the verbatim snippet the model cited must survive unmodified"
    );
    assert!(
        f.detail.contains("src/config.rs") && f.detail.contains("REQUIRE_WEBHOOK_SIGNATURE"),
        "the finding's own text must carry the CROSS-FILE evidence (the config file + flag \
         name), not just the handler site: {}",
        f.detail
    );

    // ═══ The real cross-family/cross-reference merge pass runs over it (singleton group — ═══
    // nothing to merge WITH, since there is no deterministic sibling — but this is the exact
    // function production code calls next, not a hand-simulated shape).
    let merged = ai_audit::merge_semantic_groups(findings, &files);
    assert_eq!(
        merged.len(),
        1,
        "a singleton group must still emit its one row, never vanish"
    );

    // ═══ Assemble the ledger exactly as `onboard::audit_repos` does: fold every stage sample ═══
    // the audit returned into a fresh `ScanLedger`.
    let mut ledger = ScanLedger::new();
    for sample in stage_samples {
        ledger.record_stage_sample(sample);
    }
    assert_eq!(
        ledger.total_unaccounted(),
        0,
        "every stage `audit_repo` touched must reconcile with zero unaccounted rows: {:#?}",
        ledger.stages()
    );

    // ═══ Through the real report-build pipeline. ═══
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        corpus_errors.is_empty(),
        "the bundled rule corpus must load cleanly: {corpus_errors:?}"
    );

    let report = minimal_report(merged, ledger);
    let opts = ReportOptions {
        client_name: "Acme Corp".to_string(),
        project_title: "Cross-file semantic-tier backstop E2E".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        ..Default::default()
    };
    let json = report_export::build_report_json(&report, &HashMap::new(), Some(&corpus), &opts);

    // The row must be VISIBLE somewhere in the export — never silently dropped. A critical,
    // uncited (no grounded-citation vocabulary) AI-tier finding routes to the severity-
    // unbounded `held` bucket (C5-7), with its own site in `held_for_review_findings` (C6-B1) —
    // but the invariant under test is survival, so this also accepts `curated_findings` /
    // `matrix.informational` in case a future corpus/classification change grounds this same
    // defect shape differently; what it must NEVER do is appear in none of them.
    let in_held_bucket = json.matrix.held.iter().any(|r| r.path == HANDLER_PATH);
    let in_curated = json
        .curated_findings
        .iter()
        .any(|g| g.sites.iter().any(|s| s.path == HANDLER_PATH));
    let in_held_section = json
        .held_for_review_findings
        .iter()
        .any(|g| g.sites.iter().any(|s| s.path == HANDLER_PATH));
    let in_informational = json
        .matrix
        .informational
        .iter()
        .any(|r| r.path == HANDLER_PATH);
    assert!(
        in_held_bucket || in_curated || in_informational,
        "the cross-file finding must survive SOMEWHERE in the export's matrix: {:?}",
        json.matrix
    );
    assert!(
        in_held_section || in_curated,
        "the cross-file finding must have its own renderable site in curated_findings or \
         held_for_review_findings, never counted-but-unrendered: curated={:?} held={:?}",
        json.curated_findings,
        json.held_for_review_findings
    );
    // C5-7's specific, documented destination for exactly this shape (critical + uncited):
    // assert it directly, not just "survives somewhere," so a regression that re-routes it to
    // `informational` (silently capping its visible severity) is caught.
    assert!(
        in_held_bucket && in_held_section,
        "a critical, uncited AI-tier finding must route to the severity-unbounded `held` \
         bucket, not be capped into `informational`: matrix.held={:?} held_for_review={:?}",
        json.matrix.held,
        json.held_for_review_findings
    );
}
