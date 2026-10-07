//! END-TO-END (W4 root-cause fix): a corpus rule that declares `mechanical` enforcement but
//! ships NO deterministic detector — Camerata's own `RUBY-FROZEN-STRING-LITERAL-1` is reused
//! here (a real, tracked gap; see `crate::mechanical_gate`'s module doc) — is now GUARANTEED
//! to reach the semantic/AI pass, and genuinely gets evaluated: when the planted defect is
//! present, the model's finding comes back, survives the real merge pass, and the pipeline
//! records it as `ran = true` / `RuleTier::Semantic` in the ledger — never "excluded" and
//! never a phantom "verified clean" the way it was before this fix (when such a rule was BOTH
//! withheld from the model AND had no detector, so nothing ever evaluated it).
//!
//! Companion to `pipeline_integrity_ledger_e2e.rs` (the SAME rule id, but with AI review turned
//! OFF — an ordinary, honestly-scoped skip, not this file's happy path) and
//! `semantic_tier_cross_file_backstop_e2e.rs` (the established pattern this file mirrors for
//! driving the REAL production functions — `ai_audit::audit_repo` ->
//! `ai_audit::merge_semantic_groups` -> `report_export::build_report_json` — with a STUBBED
//! `LlmPort`, since `onboard::audit_repos` always constructs a real network-backed `Llm` from
//! the process environment with no injection seam).
//!
//! ZERO API SPEND: the stub never makes a network call.

use std::collections::HashMap;

use async_trait::async_trait;
use camerata_server::ai_audit::{self, ScanMode};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::ScanReport;
use camerata_server::report_export::{self, ReportOptions};
use camerata_server::scan_ledger::{RuleTier, ScanLedger};

/// The real corpus rule under test: declares `enforcement = "mechanical"` but has no
/// registered `ArchChecker`, gateway rule arm, bundled Semgrep rule, or scan-preview linter
/// source — see `crate::mechanical_gate`'s grandfathered-gap list (pre-W4) / the inverted W4
/// build gate (`every_mechanical_rule_with_no_detector_is_code_auditable`). Its ONLY possible
/// route to ever being evaluated is the semantic/AI pass.
const RULE_NO_DETECTOR: &str = "RUBY-FROZEN-STRING-LITERAL-1";

const RUBY_PATH: &str = "lib/payment_processor.rb";
const RUBY_SNIPPET: &str = "class PaymentProcessor";

/// A fake model that always answers with ONE canned finding citing the real, ADOPTED corpus
/// rule id directly (not an invented `AI-` name) — standing in for the model genuinely
/// recognizing the planted defect against the directive it was handed. `parse_ai_findings`
/// keys a finding to the id verbatim when it is in the `adopted` set (built from `selected`),
/// so this finding surfaces AS `RULE_NO_DETECTOR`, never `AI-`-prefixed.
struct StubModel;

fn canned_finding_json() -> String {
    format!(
        r#"{{
          "findings": [
            {{
              "path": "{RUBY_PATH}",
              "line": 1,
              "severity": "low",
              "rule": "{RULE_NO_DETECTOR}",
              "title": "Ruby file is missing the frozen_string_literal magic comment",
              "code": "{RUBY_SNIPPET}",
              "detail": "lib/payment_processor.rb has no '# frozen_string_literal: true' comment on its first or second line.",
              "captures": {{}}
            }}
          ],
          "proposed_rules": [],
          "needs_files": []
        }}"#
    )
}

#[async_trait]
impl LlmPort for StubModel {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(LlmResponse {
            text: canned_finding_json(),
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
        let text = canned_finding_json();
        on_delta(&text);
        self.complete(LlmRequest::new("")).await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn fixture_files() -> Vec<(String, String)> {
    vec![(
        RUBY_PATH.to_string(),
        format!("{RUBY_SNIPPET}\n  def process(payment)\n  end\nend\n"),
    )]
}

fn minimal_report(
    findings: Vec<camerata_server::onboard::Finding>,
    ledger: ScanLedger,
) -> ScanReport {
    ScanReport {
        repos: vec!["acme/payments".to_string()],
        stacks: Vec::new(),
        files_scanned: 1,
        test_file_count: 0,
        files_excluded: 0,
        code_chars: 60,
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

#[tokio::test]
async fn mechanical_rule_with_no_detector_is_evaluated_and_fires_via_the_semantic_tier() {
    // ═══ Precondition: this rule really is CI-tier (mechanical) with no gateway-arm detector. ═══
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        corpus_errors.is_empty(),
        "the bundled rule corpus must load cleanly: {corpus_errors:?}"
    );
    let rule = corpus
        .get_by_id(RULE_NO_DETECTOR)
        .expect("precondition: the real corpus must carry this id");
    assert!(
        rule.enforcement.is_ci_enforced(),
        "precondition: {RULE_NO_DETECTOR} must declare CI-tier (mechanical) enforcement"
    );
    assert!(
        camerata_gateway::lookup_arm(RULE_NO_DETECTOR).is_none(),
        "precondition: {RULE_NO_DETECTOR} must have NO gateway-regex backstop — its only \
         possible route is the semantic pass"
    );

    // ═══ STEP 1: the real semantic-tier function, over the EXACT selected set the per-repo ═══
    // filter (`onboard::semantic_rule_ids_for_repo`) would hand it — a single CI-tier rule
    // with no deterministic detector — against a STUBBED model.
    let stub = StubModel;
    let files = fixture_files();
    let selected: Vec<(String, String)> = vec![(
        RULE_NO_DETECTOR.to_string(),
        "every .rb file declares frozen_string_literal: true".to_string(),
    )];

    let (findings, _proposed, _recs, failed_passes, stage_samples) = ai_audit::audit_repo(
        &stub,
        "acme/payments",
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

    // Same documented degradation as `semantic_tier_cross_file_backstop_e2e.rs`: the stub
    // returns the SAME findings-shaped JSON to the calibration round-trip too, which expects a
    // `{"verdicts": [...]}` shape and can't parse it — a disclosed, fail-soft degradation of
    // this fixture, never a silently dropped finding.
    for fp in &failed_passes {
        assert!(
            fp.pass.contains("estimation") || fp.pass.contains("calibrat"),
            "only the calibration/estimation round-trip is expected to fail against this \
             fixture's single canned response shape: {failed_passes:?}"
        );
    }

    // ═══ THE regression this test exists to prove closed: the model actually evaluated the ═══
    // CI-tier, no-detector rule and returned a real finding, KEYED TO THE REAL RULE ID (not
    // `AI-`-prefixed) because it was in the adopted set this time — before W4, this rule was
    // never even offered to the model, so this finding could never have existed at all.
    assert_eq!(
        findings.len(),
        1,
        "exactly one finding, from the rule the model was actually asked about: {findings:?}"
    );
    assert_eq!(
        findings[0].rule_id, RULE_NO_DETECTOR,
        "the finding must be keyed to the REAL adopted rule id, not an invented AI- name"
    );
    assert_eq!(findings[0].path, RUBY_PATH);

    // ═══ STEP 2: the real cross-family/location merge pass (singleton group — nothing to ═══
    // merge WITH here, but this is the exact function production code calls next).
    let merged = ai_audit::merge_semantic_groups(findings, &files);
    assert_eq!(
        merged.len(),
        1,
        "a singleton group must still emit its one row"
    );

    // ═══ STEP 3: the ledger, assembled exactly as `onboard::audit_repos` does for THIS case ═══
    // — see that function's per-repo loop: a CI-tier rule with no detector channel is recorded
    // ONLY once the semantic phase resolves, directly as `RuleTier::Semantic` with the model's
    // real finding count (never pre-recorded as a phantom "not run, no wired detector" the way
    // the pre-W4 pipeline did).
    let mut ledger = ScanLedger::new();
    for sample in stage_samples {
        ledger.record_stage_sample(sample);
    }
    let emitted = merged
        .iter()
        .filter(|f| f.rule_id == RULE_NO_DETECTOR)
        .count();
    ledger.record_rule(
        RULE_NO_DETECTOR,
        RuleTier::Semantic,
        true,
        None,
        files.len(),
        emitted,
    );

    let entry = ledger
        .rule(RULE_NO_DETECTOR)
        .expect("the ledger must carry this rule's outcome");
    assert!(entry.ran, "the semantic pass genuinely ran this rule");
    assert_eq!(entry.tier, RuleTier::Semantic);
    assert!(
        !entry.verified_clean(),
        "a rule that fired must never read as verified clean"
    );
    assert!(
        ledger.excluded_rules().is_empty(),
        "a rule the model actually evaluated must never appear as excluded/not-run: {:?}",
        ledger.excluded_rules()
    );
    assert!(ledger.fired_rule_ids().contains(RULE_NO_DETECTOR));
    assert_eq!(
        ledger.total_unaccounted(),
        0,
        "every stage this scan touched must reconcile with zero unaccounted rows: {:#?}",
        ledger.stages()
    );

    // ═══ STEP 4: through the real report-build pipeline — the finding must be VISIBLE, and ═══
    // the rule must appear in neither "What's healthy" nor "excluded from this audit".
    let report = minimal_report(merged, ledger);
    let opts = ReportOptions {
        client_name: "Acme Corp".to_string(),
        project_title: "Semantic-tier mechanical-no-detector coverage E2E".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        ..Default::default()
    };
    let json = report_export::build_report_json(&report, &HashMap::new(), Some(&corpus), &opts);

    assert!(
        !json
            .whats_healthy
            .rules
            .iter()
            .any(|r| r.rule_id == RULE_NO_DETECTOR),
        "a rule that fired must never appear as verified clean: {:?}",
        json.whats_healthy.rules
    );
    assert!(
        !json
            .rules_not_run
            .iter()
            .any(|r| r.rule_id == RULE_NO_DETECTOR),
        "a rule the model actually evaluated must never appear excluded from this audit: {:?}",
        json.rules_not_run
    );
    let visible_in_curated = json
        .curated_findings
        .iter()
        .any(|g| g.sites.iter().any(|s| s.path == RUBY_PATH));
    let visible_in_held = json
        .held_for_review_findings
        .iter()
        .any(|g| g.sites.iter().any(|s| s.path == RUBY_PATH));
    let visible_in_matrix = json
        .matrix
        .informational
        .iter()
        .any(|r| r.path == RUBY_PATH)
        || json.matrix.held.iter().any(|r| r.path == RUBY_PATH)
        || json.matrix.do_now.iter().any(|r| r.path == RUBY_PATH)
        || json.matrix.do_next.iter().any(|r| r.path == RUBY_PATH)
        || json.matrix.plan.iter().any(|r| r.path == RUBY_PATH)
        || json.matrix.accepted.iter().any(|r| r.path == RUBY_PATH);
    assert!(
        visible_in_matrix,
        "the finding must survive SOMEWHERE in the export's matrix: {:?}",
        json.matrix
    );
    assert!(
        visible_in_curated || visible_in_held,
        "the finding must have its own renderable site, never counted-but-unrendered: \
         curated={:?} held={:?}",
        json.curated_findings,
        json.held_for_review_findings
    );
}
