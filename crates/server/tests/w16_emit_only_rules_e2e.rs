//! END-TO-END (W16): a rule the corpus declares `inspectable = false` is EMITTED as a
//! directive but never SCANNED, and therefore can never be claimed "verified clean" against a
//! client's codebase.
//!
//! # The defect this file exists to close, stated precisely
//!
//! All 23 agentic rules (`SPIRIT-*`, `ORCH-*`, `AGENTIC-*`) arm on every unattended scan (the
//! `agentic` domain is auto-recommended regardless of detected stack, and `universal` is
//! pipeline-selected), reach the semantic pass with no filter
//! (`onboard::semantic_rule_ids_for_repo`), return nothing from the model — their subject is
//! the AUTHORING PROCESS, not the code — and are then recorded
//! `RuleLedgerEntry::verified_clean()`, because that predicate is
//! `ran && findings_emitted == 0 && !not_applicable` and nothing ever marks them
//! `not_applicable`. 14 of the 23 carry an external source URL, so they resolve a GROUNDED
//! citation and sort FIRST into the PDF's "What's healthy" list, which caps at ten. A client
//! PDF could therefore open with `ORCH-MODEL-TIERING-1: verified clean` — a claim about how
//! *we* run our AI agents, presented as a verified property of *their* codebase.
//!
//! `test_the_whole_defect_chain_is_closed` walks that EXACT chain, link by link, through the
//! real production functions, and asserts the chain is now broken at the arming link.
//! `the_defect_chain_links_still_behave_as_documented` pins the links that have NOT changed
//! (the rule still arms into the PROPOSAL, the ledger predicate is still what it was), so a
//! future regression cannot make this file pass vacuously.
//!
//! ZERO API SPEND: the model is a stub that returns an empty finding set — the measured
//! real-world behaviour of these rules against ordinary code.

use std::collections::HashMap;

use async_trait::async_trait;
use camerata_server::ai_audit::{self, ScanMode};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::{self, ScanReport, SelectedRule};
use camerata_server::report_export::{self, DispositionWire, ReportOptions};
use camerata_server::scan_ledger::{RuleTier, ScanLedger};

/// The owner's own example, and the sharpest one: which model tier ran which work is a
/// property of how an orchestrator executes, invisible in any client's code. It carries an
/// external source URL, so it resolves a GROUNDED citation and sorts to the TOP of the capped
/// "What's healthy" list — the worst slot for the least meaningful claim.
const EMIT_ONLY_RULE: &str = "ORCH-MODEL-TIERING-1";

/// The counter-example from the same families: file length IS a measurable fact of a tree, so
/// this rule stays `inspectable = true` and must keep reaching the model. It is the control
/// that proves W16 narrowed the scan rather than resurrecting the W4 prefix gate.
const INSPECTABLE_SPIRIT_RULE: &str = "SPIRIT-FILE-SIZE-1";

const SRC_PATH: &str = "src/lib.rs";

/// A model that finds nothing — the measured real behaviour when it is handed a process rule
/// and a file of ordinary code. This is the whole point: "returned nothing" was
/// indistinguishable from "assessed and clean".
struct SilentModel;

const EMPTY_FINDINGS_JSON: &str = r#"{"findings": [], "proposed_rules": [], "needs_files": []}"#;

#[async_trait]
impl LlmPort for SilentModel {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(LlmResponse {
            text: EMPTY_FINDINGS_JSON.to_string(),
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
        on_delta(EMPTY_FINDINGS_JSON);
        self.complete(LlmRequest::new("")).await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn fixture_files() -> Vec<(String, String)> {
    vec![(
        SRC_PATH.to_string(),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n".to_string(),
    )]
}

fn minimal_report(ledger: ScanLedger, audited_rule_ids: Vec<String>) -> ScanReport {
    let provenance = onboard::ScanProvenance {
        audited_rule_ids,
        ..onboard::ScanProvenance::default()
    };
    ScanReport {
        repos: vec!["acme/api".to_string()],
        stacks: Vec::new(),
        files_scanned: 1,
        test_file_count: 0,
        files_excluded: 0,
        code_chars: 48,
        code_lines: 3,
        code_lines_by_language: Vec::new(),
        excluded_mechanical_rules: Vec::new(),
        findings: Vec::new(),
        proposed_rules: Vec::new(),
        gated: false,
        blocked: false,
        ai_blocked_reason: None,
        ai_error: None,
        message: None,
        actual_usage: None,
        deep: None,
        coverage_notes: Vec::new(),
        provenance,
        recommendations: HashMap::new(),
        failed_passes: Vec::new(),
        ledger,
    }
}

/// The healthy-rule ids the REAL exporter would render into the PDF's "What's healthy" block.
fn healthy_ids_in_export(report: ScanReport, corpus: &camerata_rules::RuleSet) -> Vec<String> {
    let json = report_export::build_report_json(
        &report.seal(),
        &HashMap::<String, DispositionWire>::new(),
        Some(corpus),
        &ReportOptions::default(),
    );
    json.whats_healthy
        .rules
        .iter()
        .map(|r| r.rule_id.clone())
        .collect()
}

/// req_w16_1 / req_w16_2 / req_w16_3: the defect chain, link by link, through the real
/// production functions — arming, the semantic prompt, the ledger, the export.
#[tokio::test]
async fn req_w16_emit_only_rule_never_reaches_the_scan_or_the_healthy_list() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(
        corpus_errors.is_empty(),
        "the bundled rule corpus must load cleanly: {corpus_errors:?}"
    );

    // ═══ Precondition: both rules are REAL corpus rules, and the corpus declares exactly ═══
    // the split this test is about.
    let emit_only = corpus
        .get_by_id(EMIT_ONLY_RULE)
        .expect("precondition: the real corpus must carry the emit-only rule");
    let scannable = corpus
        .get_by_id(INSPECTABLE_SPIRIT_RULE)
        .expect("precondition: the real corpus must carry the control rule");
    assert!(
        !emit_only.is_inspectable(),
        "precondition: {EMIT_ONLY_RULE} must be declared `inspectable = false`"
    );
    assert!(
        scannable.is_inspectable(),
        "precondition: {INSPECTABLE_SPIRIT_RULE} must stay `inspectable = true` — W16 narrows \
         the W4 ruling, it does not resurrect the prefix gate"
    );
    // The thing that made this defect CLIENT-FACING rather than merely untidy: an external
    // source URL resolves a grounded citation, which sorts first into the capped healthy list.
    assert!(
        emit_only.sources.iter().any(|s| s.url.starts_with("http")),
        "precondition: {EMIT_ONLY_RULE} carries an external source URL, which is why it sorted \
         to the TOP of the capped 'What's healthy' list"
    );

    // ═══ LINK 1 (unchanged, deliberately): the rule still ARMS. ═══════════════════════════
    // `inspectable = false` means "emitted, never scanned" — it must stay in the proposal and
    // stay auto-recommended, because the EMIT half of its life is correct and valuable.
    let proposed = onboard::propose_corpus_rules(&[(
        "acme/api".to_string(),
        vec!["rust".to_string(), "universal".to_string()],
    )])
    .await;
    let armed = proposed
        .iter()
        .find(|p| p.id == EMIT_ONLY_RULE)
        .expect("the emit-only rule must still be PROPOSED — it is still an adopted directive");
    assert!(
        armed.is_auto_recommended,
        "the emit-only rule must still auto-arm on an unattended scan: its directive still \
         governs future work, which is the half W16 must not touch"
    );

    // ═══ LINK 2 (the fix): it must NOT reach the semantic prompt. ═════════════════════════
    let selected = vec![
        SelectedRule {
            id: EMIT_ONLY_RULE.to_string(),
            directive: "cheap models for bulk work, top-tier for decisions".to_string(),
            repos: Vec::new(),
        },
        SelectedRule {
            id: INSPECTABLE_SPIRIT_RULE.to_string(),
            directive: "hand-maintained files are sized for human reading".to_string(),
            repos: Vec::new(),
        },
    ];
    let scan_set = onboard::inspectable_rules_for_scan(&selected, Some(&corpus));
    let scan_ids: Vec<&str> = scan_set.iter().map(|r| r.id.as_str()).collect();
    assert!(
        !scan_ids.contains(&EMIT_ONLY_RULE),
        "RED BEFORE / GREEN AFTER: {EMIT_ONLY_RULE} must be excluded BEFORE arming, so it \
         never enters the ledger's accounting at all. Got: {scan_ids:?}"
    );
    assert!(
        scan_ids.contains(&INSPECTABLE_SPIRIT_RULE),
        "the control rule must still be scanned: {scan_ids:?}"
    );

    // The semantic prompt is built from the scan set, so the exclusion propagates.
    let semantic = onboard::semantic_rule_ids_for_repo_public(&scan_set, "acme/api");
    assert!(
        !semantic.iter().any(|(id, _)| id == EMIT_ONLY_RULE),
        "the emit-only rule must never be handed to the model: {semantic:?}"
    );

    // ═══ LINK 3: the model pass, driven for real against a model that finds nothing. ══════
    let files = fixture_files();
    let (findings, _proposed, _recs, _failed, _samples) = ai_audit::audit_repo(
        &SilentModel,
        "acme/api",
        &files,
        &semantic,
        &[], // alternatives
        &HashMap::new(),
        None,
        None,
        ScanMode::Sequential,
        false,
        None,
        None,
        None,
        Some(&files),
    )
    .await
    .expect("audit_repo must succeed against the stub model");
    assert!(
        findings.is_empty(),
        "the silent model returns nothing — that is the measured behaviour the defect rode on"
    );

    // ═══ LINK 4: the ledger, recorded EXACTLY as `onboard::audit_repos` records it. ═══════
    // (Its per-repo loop iterates the `semantic` set and calls `record_rule(.., true, None,
    // files.len(), emitted)` for each id. The emit-only rule is not in that set any more, so
    // it is never recorded at all — which is the point: a rule that armed and was then
    // suppressed would still pollute the accounting.)
    let mut ledger = ScanLedger::new();
    for (rid, _) in &semantic {
        ledger.record_rule(rid.clone(), RuleTier::Semantic, true, None, files.len(), 0);
    }
    assert!(
        ledger.rule(EMIT_ONLY_RULE).is_none(),
        "the emit-only rule must have NO ledger entry — not a clean one, not an excluded one"
    );
    assert!(
        !ledger
            .healthy_rule_ids()
            .iter()
            .any(|r| r == EMIT_ONLY_RULE),
        "the emit-only rule must never be 'verified clean': {:?}",
        ledger.healthy_rule_ids()
    );
    assert!(
        !ledger
            .excluded_rules()
            .iter()
            .any(|(id, _)| *id == EMIT_ONLY_RULE),
        "nor may it appear in 'rules not run this scan' — it was never a scan rule"
    );
    assert!(
        ledger
            .healthy_rule_ids()
            .iter()
            .any(|r| r == INSPECTABLE_SPIRIT_RULE),
        "the control rule genuinely ran and found nothing, so it IS verified clean"
    );

    // ═══ LINK 5: the client-facing export. ════════════════════════════════════════════════
    let audited: Vec<String> = scan_set.iter().map(|r| r.id.clone()).collect();
    assert!(
        !audited.contains(&EMIT_ONLY_RULE.to_string()),
        "`provenance.audited_rule_ids` is built from the scan set, so the Coverage sheet's \
         per-rule rows and the G3 'we checked N rules' count both shed it"
    );
    let report = minimal_report(ledger, audited);
    let healthy = healthy_ids_in_export(report, &corpus);
    assert!(
        !healthy.contains(&EMIT_ONLY_RULE.to_string()),
        "THE CLIENT-FACING CLAIM: {EMIT_ONLY_RULE} must never render in 'What's healthy'. \
         Got: {healthy:?}"
    );
    assert!(
        healthy.contains(&INSPECTABLE_SPIRIT_RULE.to_string()),
        "the control rule must still render as healthy: {healthy:?}"
    );
}

/// req_w16_4: the EMIT half is untouched. This is the half most likely to break silently, and
/// breaking it would delete client value rather than noise — an `inspectable = false` rule
/// must still be installed into the client's `AGENTS.md` / `CONVENTIONS.md` as a directive
/// governing future work.
#[tokio::test]
async fn req_w16_emit_only_rule_is_still_written_into_agents_md() {
    let corpus_path = camerata_rules::corpus_path();
    let (corpus, corpus_errors) = camerata_rules::load_corpus_lenient(&corpus_path).await;
    assert!(corpus_errors.is_empty(), "{corpus_errors:?}");

    let rule = corpus.get_by_id(EMIT_ONLY_RULE).expect("corpus rule");
    assert!(!rule.is_inspectable(), "precondition");
    assert_eq!(
        rule.enforcement.as_str(),
        "prose",
        "precondition: this rule emits to AGENTS.md, which is the file under test"
    );

    // A structured emit-only rule too, so both emit destinations are covered.
    let structured = corpus
        .get_by_id("ORCH-REVIEWER-SPLIT-1")
        .expect("corpus rule");
    assert!(!structured.is_inspectable(), "precondition");
    assert_eq!(structured.enforcement.as_str(), "structured");

    let arm_rules = [
        camerata_server::arm::ArmRule {
            id: rule.id.0.clone(),
            title: rule.title.clone(),
            directive: "Cheap models for bulk work; top-tier models for decisions.".to_string(),
            enforcement: rule.enforcement.as_str().to_string(),
            scope: "repo-local".to_string(),
            repos: Vec::new(),
            conformance: None,
            option: None,
        },
        camerata_server::arm::ArmRule {
            id: structured.id.0.clone(),
            title: structured.title.clone(),
            directive: "The reviewer agent is distinct from the author agent.".to_string(),
            enforcement: structured.enforcement.as_str().to_string(),
            scope: "repo-local".to_string(),
            repos: Vec::new(),
            conformance: None,
            option: None,
        },
    ];
    let refs: Vec<&camerata_server::arm::ArmRule> = arm_rules.iter().collect();
    let files = camerata_server::arm::arm_files_for_repo(&refs, &[]);

    let agents = files
        .iter()
        .find(|(name, _)| name == "AGENTS.md")
        .map(|(_, body)| body.clone())
        .expect("AGENTS.md must still be emitted for a prose emit-only rule");
    assert!(
        agents.contains(EMIT_ONLY_RULE),
        "an `inspectable = false` prose rule MUST still reach the client's AGENTS.md — the \
         emit path is unchanged by W16. AGENTS.md was:\n{agents}"
    );
    assert!(
        agents.contains("Cheap models for bulk work"),
        "its DIRECTIVE text, not just its id, must be installed"
    );

    let conventions = files
        .iter()
        .find(|(name, _)| name == "CONVENTIONS.md")
        .map(|(_, body)| body.clone())
        .expect("CONVENTIONS.md must still be emitted for a structured emit-only rule");
    assert!(
        conventions.contains("ORCH-REVIEWER-SPLIT-1"),
        "an `inspectable = false` structured rule MUST still reach CONVENTIONS.md:\n{conventions}"
    );

    // And the gate config, which is the third emit artifact.
    let gate = files
        .iter()
        .find(|(name, _)| name == ".camerata/rules.json")
        .map(|(_, body)| body.clone())
        .expect(".camerata/rules.json must still be emitted");
    assert!(
        gate.contains(EMIT_ONLY_RULE),
        "the emit-only rule must still be listed in the project's gate config:\n{gate}"
    );
}
