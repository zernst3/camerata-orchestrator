//! END-TO-END: a CI-tier architectural rule whose registered checker is CONFIG-GATED —
//! `ARCH-HANDLER-NO-DB-1` (`crates/checks/src/handler_no_db_checker.rs`) needs
//! `.camerata/architecture.toml` to answer deterministically — must, on a repo with NO
//! architecture config, be recorded by the ledger with an ACCURATE, honest disclosure (never
//! the `NO_WIRED_DETECTOR_REASON` shape — a real checker IS registered for this id) AND still
//! genuinely reach the semantic/AI pass, which is its only possible route to evaluation on an
//! unconfigured repo.
//!
//! Mirrors `semantic_tier_covers_mechanical_no_detector_rule_e2e.rs`'s established pattern for
//! driving the REAL production functions (`onboard::architectural::audit_architectural` for the
//! deterministic phase, `ai_audit::audit_repo` for the semantic phase, then the SAME ledger
//! construction `onboard::audit_repos`'s per-repo loop performs) with a STUBBED `LlmPort`, since
//! `onboard::audit_repos` always constructs a real network-backed `Llm`.
//!
//! ZERO API SPEND: the stub never makes a network call.

use std::collections::HashMap;

use async_trait::async_trait;
use camerata_checks::arch_checker::{ledger_detector_rule_ids_for_repo, RepoView};
use camerata_server::ai_audit::{self, ScanMode};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::architectural::audit_architectural;
use camerata_server::onboard::ScanReport;
use camerata_server::report_export::{self, ReportOptions};
use camerata_server::scan_ledger::{RuleTier, ScanLedger, NO_WIRED_DETECTOR_REASON};

/// The real corpus rule under test: declares `enforcement = "architectural"`, IS answered by a
/// registered checker (`camerata_checks::handler_no_db_checker::HandlerNoDbChecker`), but that
/// checker is config-gated — see `crates/checks/src/handler_no_db_checker.rs`'s module doc.
const RULE_CONFIG_GATED: &str = "ARCH-HANDLER-NO-DB-1";

const HANDLER_PATH: &str = "src/routes/users.rs";
// No embedded double quotes: this snippet is interpolated verbatim into a JSON string value by
// `canned_finding_json` below (via `{HANDLER_SNIPPET}`), not JSON-escaped — a literal `"` here
// would prematurely terminate that JSON string and make the canned response unparseable.
const HANDLER_SNIPPET: &str = "fn get_user(db: &Pool) { db.query(QUERY); }";

struct StubModel;

fn canned_finding_json() -> String {
    format!(
        r#"{{
          "findings": [
            {{
              "path": "{HANDLER_PATH}",
              "line": 1,
              "severity": "high",
              "rule": "{RULE_CONFIG_GATED}",
              "title": "Handler queries the database directly",
              "code": "{HANDLER_SNIPPET}",
              "detail": "get_user calls db.query directly instead of delegating to a repository.",
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

/// Deliberately NO `.camerata/architecture.toml` anywhere in this file set.
fn fixture_files() -> Vec<(String, String)> {
    vec![(HANDLER_PATH.to_string(), format!("{HANDLER_SNIPPET}\n"))]
}

fn minimal_report(
    findings: Vec<camerata_server::onboard::Finding>,
    ledger: ScanLedger,
) -> ScanReport {
    ScanReport {
        repos: vec!["acme/api".to_string()],
        stacks: Vec::new(),
        files_scanned: 1,
        test_file_count: 0,
        files_excluded: 0,
        code_chars: 60,
        code_lines: 1,
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
async fn config_gated_rule_with_no_architecture_config_gets_an_accurate_note_and_still_reaches_the_semantic_tier(
) {
    // ═══ Precondition: this rule really is CI-tier (architectural) and DOES have a ═══
    // registered checker — the opposite of a genuine "no wired detector" phantom.
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        corpus_errors.is_empty(),
        "the bundled rule corpus must load cleanly: {corpus_errors:?}"
    );
    let rule = corpus
        .get_by_id(RULE_CONFIG_GATED)
        .expect("precondition: the real corpus must carry this id");
    assert!(
        rule.enforcement.is_ci_enforced(),
        "precondition: {RULE_CONFIG_GATED} must declare CI-tier enforcement"
    );
    assert!(
        camerata_checks::arch_checker::any_registered_checker_answers(RULE_CONFIG_GATED),
        "precondition: a real checker must be registered for this id"
    );

    let files = fixture_files();
    let repo_view = RepoView {
        spec: "acme/api",
        files: &files,
    };

    // ═══ STEP 1: the real deterministic architectural pass, over a repo with NO ═══
    // `.camerata/architecture.toml` — the checker must genuinely abstain (zero findings), and
    // the per-repo ledger-credit set must NOT include this id.
    let armed: std::collections::HashSet<&str> = [RULE_CONFIG_GATED].into_iter().collect();
    let arch_findings = audit_architectural("acme/api", &files, &armed);
    assert!(
        arch_findings.is_empty(),
        "the config-gated checker must abstain entirely without architecture config: \
         {arch_findings:?}"
    );
    let ledger_ids = ledger_detector_rule_ids_for_repo(&repo_view);
    assert!(
        !ledger_ids.contains(RULE_CONFIG_GATED),
        "an unconfigured repo must not ledger-credit this config-gated id: {ledger_ids:?}"
    );

    // ═══ STEP 2: the deterministic-phase ledger recording `onboard::audit_repos` performs for ═══
    // exactly this shape — no detector channel, but a real checker exists, so the honest
    // disclosure is QUEUED (not yet attached to any entry — none exists yet).
    let mut ledger = ScanLedger::new();
    assert!(ledger.rule(RULE_CONFIG_GATED).is_none());
    ledger.note_config_gated_rule(RULE_CONFIG_GATED);

    // ═══ STEP 3: the real semantic-tier function against a STUBBED model, over the EXACT ═══
    // selected set the per-repo filter would hand it for this unconfigured repo — nothing
    // excludes this id from the semantic prompt (W4), config-gated or not.
    let stub = StubModel;
    let selected: Vec<(String, String)> = vec![(
        RULE_CONFIG_GATED.to_string(),
        "handlers never touch the database directly".to_string(),
    )];
    let (findings, _proposed, _recs, failed_passes, stage_samples) = ai_audit::audit_repo(
        &stub,
        "acme/api",
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
    for fp in &failed_passes {
        assert!(
            fp.pass.contains("estimation") || fp.pass.contains("calibrat"),
            "only the calibration/estimation round-trip is expected to fail against this \
             fixture's single canned response shape: {failed_passes:?}"
        );
    }
    assert_eq!(findings.len(), 1, "{findings:?}");
    assert_eq!(findings[0].rule_id, RULE_CONFIG_GATED);

    let merged = ai_audit::merge_semantic_groups(findings, &files);
    assert_eq!(merged.len(), 1);

    // ═══ STEP 4: the semantic-phase ledger recording settles `ran`/`findings_emitted` — the ═══
    // SAME `record_rule` call `onboard::audit_repos` makes once `audit_repo` returns.
    for sample in stage_samples {
        ledger.record_stage_sample(sample);
    }
    let emitted = merged
        .iter()
        .filter(|f| f.rule_id == RULE_CONFIG_GATED)
        .count();
    ledger.record_rule(
        RULE_CONFIG_GATED,
        RuleTier::Semantic,
        true,
        None,
        files.len(),
        emitted,
    );

    // ═══ THE regression this test exists to prove closed. ═══
    let entry = ledger
        .rule(RULE_CONFIG_GATED)
        .expect("the ledger must carry this rule's outcome");
    assert!(entry.ran, "the semantic pass genuinely evaluated it");
    assert_eq!(entry.tier, RuleTier::Semantic);
    assert!(!entry.verified_clean(), "it fired — never verified clean");
    assert_eq!(
        entry.coverage_note.as_deref(),
        Some(camerata_server::scan_ledger::CONFIG_GATED_NOTE),
        "must carry the honest config-gated disclosure, not silently read as an ordinary \
         prose-only rule: {entry:?}"
    );
    if let Some(reason) = &entry.skip_reason {
        assert!(
            !reason.contains(NO_WIRED_DETECTOR_REASON),
            "must NEVER be classified as having no wired detector — a real checker exists for \
             this id: {reason}"
        );
    }
    assert!(
        ledger.excluded_rules().is_empty(),
        "a rule the model actually evaluated must never appear as excluded/not-run: {:?}",
        ledger.excluded_rules()
    );
    assert!(ledger.fired_rule_ids().contains(RULE_CONFIG_GATED));
    assert_eq!(
        ledger.total_unaccounted(),
        0,
        "every stage this scan touched must reconcile with zero unaccounted rows: {:#?}",
        ledger.stages()
    );

    // ═══ STEP 5: through the real report-build pipeline — never reported as a mechanical ═══
    // check that passed, never excluded.
    let report = minimal_report(merged, ledger);
    let opts = ReportOptions {
        client_name: "Acme Corp".to_string(),
        project_title: "Config-gated architectural rule semantic coverage E2E".to_string(),
        prepared_by: "Camerata (automated e2e test)".to_string(),
        ..Default::default()
    };
    let json = report_export::build_report_json(&report, &HashMap::new(), Some(&corpus), &opts);
    assert!(
        !json
            .whats_healthy
            .rules
            .iter()
            .any(|r| r.rule_id == RULE_CONFIG_GATED),
        "a rule that fired must never appear as verified clean: {:?}",
        json.whats_healthy.rules
    );
    assert!(
        !json
            .rules_not_run
            .iter()
            .any(|r| r.rule_id == RULE_CONFIG_GATED),
        "a rule the model actually evaluated must never appear excluded from this audit: {:?}",
        json.rules_not_run
    );
}
