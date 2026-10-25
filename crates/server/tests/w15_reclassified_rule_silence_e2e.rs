//! A5.7 / W15 — END-TO-END proof that `ARCH-OBSERVABILITY-SILENT-FAILURE-1` stopped being
//! silent on the fault its own prose names.
//!
//! # What was wrong
//!
//! The rule shipped as `kind = "decision"` with ZERO `violates` flags and exactly two
//! `[[option]]` blocks, the second of which — `allow-silent-discards` — opens its `why` with
//! the literal token `Rejected:`. That is the identical shape W12 found on nineteen security
//! rules: a defect and its degenerate form wearing the `[[option]]` shape of a free choice.
//! Fed a codebase that matches the rejected option, the alternatives pass took
//! `apply_alternative_decision`'s `!option.violates` branch — returning no finding, REWRITING
//! the project's effective directive to "catch blocks may be left empty", and then letting
//! `conventions_observed_findings` emit a `ConventionObserved` row asserting the codebase
//! "consistently follows" that convention with zero violations.
//!
//! # The honest scope of this proof (read before trusting it)
//!
//! `ARCH-OBSERVABILITY-SILENT-FAILURE-1` declares `enforcement = "architectural"`, which is
//! CI-tier (`EnforcementKind::is_ci_enforced`). `onboard::build_rule_alternatives` filters
//! through `is_semantic_multi_option_rule`, which excludes every CI-tier rule — so on a NORMAL
//! full scan today this rule is never handed to `audit_repo` as an alternative at all, and the
//! `kind`/`violates` pair is INERT for it on that path. That is exactly the caveat W14 already
//! recorded in prose for the mechanical sibling `SEC-NO-RAW-SQL-CONCAT-1`
//! (`crates/rules/tests/w14_corpus_schema_gates.rs`): *"a CI-tier rule never enters the
//! alternatives pass, so the flag would be inert today ... but the classification contradicts
//! the rule's own prose, and the silence becomes real the moment its enforcement tier changes."*
//!
//! So this file proves TWO distinct things and keeps them apart:
//!
//! 1. [`silent_failure_rule_is_ci_tier_so_a_normal_scan_never_offers_it_as_a_choice`] — the
//!    standing precondition, asserted rather than assumed, so that if this rule's enforcement
//!    tier is ever lowered to `structured`/`prose` the test that says "inert" starts failing
//!    and whoever lowered it is sent to read the other test in this file.
//! 2. [`req_w15_silent_failure_reports_its_discard_option_instead_of_recording_a_convention`]
//!    — the real behavioral change, driven through the REAL public `ai_audit::audit_repo` with
//!    the rule's REAL corpus options. The alternatives entry is constructed here rather than
//!    produced by `build_rule_alternatives` (which would skip it, per (1)); that is the same
//!    input the `rescan-alternatives` endpoint builds, and the input a normal scan WOULD build
//!    the moment the tier changes. Before the reclassification this test failed on every one
//!    of its three assertions; after it, the scan reports a `Defect` and records no convention.
//!
//! ZERO API SPEND: the stub model never makes a network call.

use std::collections::HashMap;

use async_trait::async_trait;
use camerata_server::ai_audit::{self, RuleAlternatives, ScanMode};
use camerata_server::llm::{LlmPort, LlmRequest, LlmResponse};
use camerata_server::onboard::RuleOptionView;

/// The reclassified rule. Its discard option is located from the corpus text at runtime (by
/// the `Rejected:` prose token), never hardcoded, so this test tracks the rule rather than a
/// snapshot of it.
const RULE_ID: &str = "ARCH-OBSERVABILITY-SILENT-FAILURE-1";

const REPO: &str = "acme/billing";
const FIXTURE_PATH: &str = "src/billing/invoice.ts";

/// The planted codebase: every error-handling site discards the caught error with no log call,
/// no propagation and no justifying comment — i.e. the codebase literally matches the
/// `allow-silent-discards` option.
fn fixture_files() -> Vec<(String, String)> {
    vec![(
        FIXTURE_PATH.to_string(),
        "export async function settle(invoiceId: string) {\n  \
         try {\n    await chargeCard(invoiceId);\n  } catch (e) {}\n  \
         try {\n    await notifyCustomer(invoiceId);\n  } catch (err) {\n    // nothing\n  }\n}\n"
            .to_string(),
    )]
}

/// A fake model that reports the repo consistently follows the DISCARD option, with a real
/// evidence citation, and finds zero per-chunk violations of its own. The empty `findings`
/// array is load-bearing: it proves any finding that comes out of `audit_repo` came from the
/// alternatives-decision mechanism itself, not from the ordinary violation pass independently
/// noticing the same empty catch block.
struct StubModel {
    discard_option_id: String,
}

impl StubModel {
    fn canned(&self) -> String {
        format!(
            r#"{{
              "recommendations": [
                {{
                  "rule_id": "{RULE_ID}",
                  "recommended_option_id": "{}",
                  "recommendation_reasoning": "every catch block in this repo is empty — no logger call, no rethrow, no justifying comment",
                  "evidence": "{FIXTURE_PATH}:4 - catch (e) {{}} discards the error with zero action",
                  "applicable": true
                }}
              ],
              "findings": [],
              "proposed_rules": [],
              "needs_files": []
            }}"#,
            self.discard_option_id
        )
    }
}

#[async_trait]
impl LlmPort for StubModel {
    async fn complete(&self, _req: LlmRequest) -> anyhow::Result<LlmResponse> {
        Ok(LlmResponse {
            text: self.canned(),
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
        _req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let text = self.canned();
        on_delta(&text);
        self.complete(LlmRequest::new("")).await
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

async fn real_rule() -> Option<camerata_rules::Rule> {
    let path = camerata_rules::corpus_path();
    if !path.exists() {
        return None; // skip without the bundled corpus checked out
    }
    let corpus = camerata_rules::load_corpus(&path)
        .await
        .expect("the real corpus loads");
    Some(
        corpus
            .get_by_id(RULE_ID)
            .expect("the reclassified rule must exist in the shipped corpus")
            .clone(),
    )
}

/// Standing precondition, asserted so it cannot silently stop being true: this rule is CI-tier,
/// which is why a normal full scan never routes it through the alternatives pass and why the
/// `violates` flag is inert on that path TODAY. If someone lowers its `enforcement` to a
/// non-CI tier, this test fails and points them at the behavioral test below, which is the one
/// that then becomes live.
#[tokio::test]
async fn silent_failure_rule_is_ci_tier_so_a_normal_scan_never_offers_it_as_a_choice() {
    let Some(rule) = real_rule().await else {
        return;
    };
    assert!(
        rule.enforcement.is_ci_enforced(),
        "{RULE_ID} is documented throughout this file as CI-tier (architectural). It is now \
         {:?}, which means `onboard::build_rule_alternatives` WILL hand it to the alternatives \
         pass on a normal scan — read \
         `req_w15_silent_failure_reports_its_discard_option_instead_of_recording_a_convention` \
         in this file: that path is now live for this rule and the `defect` kind is what keeps \
         it from adopting its own discard option as the project directive.",
        rule.enforcement
    );
}

/// req_w15: fed a codebase that matches the option its own author marked `Rejected:`,
/// `ARCH-OBSERVABILITY-SILENT-FAILURE-1` REPORTS that as a defect and leaves the project's
/// directive alone — it is never recorded as "the convention this codebase chose".
///
/// Three assertions, each with its own failure mode:
///
/// 1. The discard option carries `violates = true`. Zero flags is the original silence.
/// 2. `audit_repo` emits a `FindingKind::Defect` row keyed to the rule, with
///    `evaluated_option_id` pointing at the discard option (so `report_export::resolve_fix`
///    renders that option's remediation), even though the stub's per-chunk response is
///    `"findings": []`.
/// 3. NO `ConventionObserved` row for this rule survives into the returned finding set — the
///    literal "reported as a consistently wrong thing in the code" surface.
///
/// FAILS BEFORE THE FIX (verified by running this exact test on the unmodified TOML at
/// `238b3d7`): the rule was `kind = "decision"` with ZERO `violates` options, so (1) failed
/// outright; `apply_alternative_decision` took its `!option.violates` branch, returning `None`
/// and rewriting the effective directive to "catch/except/rescue blocks ... may be left empty",
/// so (2) found no finding at all; and with no violation trace left to find,
/// `conventions_observed_findings` emitted a `ConventionObserved` row claiming the codebase
/// consistently follows that convention, so (3) failed too.
#[tokio::test]
async fn req_w15_silent_failure_reports_its_discard_option_instead_of_recording_a_convention() {
    let Some(rule) = real_rule().await else {
        return;
    };

    // The option the rule's own author marked `Rejected:`, located from the corpus text.
    let discard = rule
        .options
        .iter()
        .find(|o| o.why.trim_start().starts_with("Rejected:"))
        .expect("the rule must still carry exactly one `Rejected:` option");

    // (1) The flag itself.
    assert!(
        discard.violates,
        "the `Rejected:` option ({}) must be marked `violates` — it describes discarding a \
         caught error with zero action, which is the fault this rule exists to report (CWE-390), \
         not an alternative a project may adopt",
        discard.id
    );

    let correct = rule
        .options
        .iter()
        .find(|o| !o.violates)
        .expect("the rule must still have a non-degenerate option");

    let stub = StubModel {
        discard_option_id: discard.id.clone(),
    };
    let files = fixture_files();
    let selected: Vec<(String, String)> = vec![(RULE_ID.to_string(), correct.directive.clone())];
    let alternatives = vec![RuleAlternatives {
        rule_id: RULE_ID.to_string(),
        options: rule
            .options
            .iter()
            .map(|o| RuleOptionView {
                id: o.id.clone(),
                label: o.label.clone(),
                directive: o.directive.clone(),
                why: o.why.clone(),
                violates: o.violates,
            })
            .collect(),
        selected_option_id: rule.default_option.clone(),
        kind: rule.kind(),
    }];

    let (findings, _proposed, recs, _failed, _stage_samples) = ai_audit::audit_repo(
        &stub,
        REPO,
        &files,
        &selected,
        &alternatives,
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

    // Fixture sanity: the alternatives pass really did decide the repo follows the discard
    // option. Without this, assertions (2)/(3) could pass vacuously on a parse failure.
    let rec = recs
        .iter()
        .find(|r| r.rule_id.eq_ignore_ascii_case(RULE_ID))
        .expect("the stub's recommendation for this rule must have parsed");
    assert_eq!(
        rec.recommended_option_id, discard.id,
        "fixture sanity: the scan must have decided the repo follows the DISCARD option"
    );

    // (2) It reports.
    let f = findings
        .iter()
        .find(|f| f.rule_id.eq_ignore_ascii_case(RULE_ID))
        .unwrap_or_else(|| {
            panic!(
                "a codebase matching the `Rejected:` option must produce a finding, not silence \
                 — got {} findings: {:?}",
                findings.len(),
                findings.iter().map(|f| &f.rule_id).collect::<Vec<_>>()
            )
        });
    assert_eq!(
        f.kind,
        Some(camerata_api_types::audit::FindingKind::Defect),
        "an objective fault, never a consistency deviation"
    );
    assert_eq!(
        f.evaluated_option_id.as_deref(),
        Some(discard.id.as_str()),
        "report_export::resolve_fix keys the remediation text off this"
    );

    // (3) And it is never recorded as an observed convention.
    use camerata_api_types::audit::FindingKind;
    let observed: Vec<&camerata_server::onboard::Finding> = findings
        .iter()
        .filter(|f| {
            f.rule_id.eq_ignore_ascii_case(RULE_ID)
                && matches!(f.kind, Some(FindingKind::ConventionObserved(_)))
        })
        .collect();
    assert!(
        observed.is_empty(),
        "a codebase matching the `Rejected:` option must NOT be reported as having adopted a \
         convention — that is the exact defect this reclassification fixes: {observed:?}"
    );
}
