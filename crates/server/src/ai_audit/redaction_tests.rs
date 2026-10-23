//! Construction-time redaction at the AI-tier producers (docs/design/2026-10-23_secret-
//! redaction-and-release-gate.md section 4.2, paths A8 to A14 and B1 to B3) and the model-prompt
//! choke point (section 5). Every secret is built at runtime by `camerata_secrets::fixtures`.

use camerata_api_types::audit::FindingKind;
use camerata_secrets::fixtures::{planted, Planted};
use camerata_secrets::{detect_secrets, SecretShape};

use super::*;
use crate::llm::{LlmPort, LlmRequest, LlmResponse};

fn secret() -> Planted {
    planted(SecretShape::SupabaseSecretKey)
}

/// Whether the planted value appears anywhere in the serialized finding.
fn leaks(f: &Finding, p: &Planted) -> bool {
    serde_json::to_string(f)
        .expect("a finding serializes")
        .contains(&p.value)
}

#[test]
fn parse_ai_findings_redacts_model_code_and_detail() {
    let p = secret();
    let raw = serde_json::json!({
        "findings": [{
            "rule": "AI-SECRETS",
            "severity": "critical",
            "title": format!("Key committed: {}", p.value),
            "code": p.text,
            "detail": format!("The line {} holds a live credential.", p.value),
            "path": ".env",
            "line": 3,
            "captures": {"bucket": p.value},
        }]
    })
    .to_string();
    let (findings, _) = parse_ai_findings("acme/app", &raw, &std::collections::HashSet::new());
    assert_eq!(findings.len(), 1);
    assert!(!leaks(&findings[0], &p), "model code or detail leaked");
    assert!(
        findings[0].snippet.contains("sb_secret_…"),
        "{}",
        findings[0].snippet
    );
}

#[test]
fn violating_option_and_convention_rows_redact_evidence_and_reasoning() {
    let p = secret();
    let option = RuleOptionView {
        id: "bad".to_string(),
        label: format!("Keys live in code {}", p.value),
        directive: "d".to_string(),
        why: "w".to_string(),
        violates: true,
    };
    let mut selected = vec![("ARCH-X-1".to_string(), "d".to_string())];
    let evidence = format!(".env:3 {}", p.text);
    let reasoning = format!("because {}", p.value);
    let violating = apply_alternative_decision(
        &mut selected,
        "acme/app",
        "ARCH-X-1",
        &option,
        &reasoning,
        Some(&evidence),
    )
    .expect("a violating option yields a finding");
    assert!(!leaks(&violating, &p), "violating-option row leaked");

    let good = RuleOptionView {
        id: "good".to_string(),
        label: format!("Keys live in a vault {}", p.value),
        directive: "d".to_string(),
        why: "w".to_string(),
        violates: false,
    };
    let alternatives = vec![RuleAlternatives {
        rule_id: "ARCH-X-1".to_string(),
        options: vec![good.clone()],
        selected_option_id: Some("good".to_string()),
        kind: None,
    }];
    let rec = RuleRecommendation {
        rule_id: "ARCH-X-1".to_string(),
        recommended_option_id: "good".to_string(),
        recommendation_reasoning: reasoning.clone(),
        evidence: Some(evidence.clone()),
        currently_followed: Some(true),
        ..RuleRecommendation::default()
    };
    let convention = conventions_observed_findings("acme/app", &[], &[rec], &alternatives);
    assert_eq!(convention.len(), 1);
    assert!(!leaks(&convention[0], &p), "convention-observed row leaked");

    let no_conv_rec = RuleRecommendation {
        rule_id: "ARCH-Y-1".to_string(),
        no_convention: true,
        recommendation_reasoning: reasoning,
        observed_options: vec![camerata_api_types::audit::ObservedOptionUsage {
            option_id: "good".to_string(),
            label: format!("Keys in a vault {}", p.value),
            sites: Some(2),
        }],
        ..RuleRecommendation::default()
    };
    let no_conv = no_convention_findings("acme/app", &[no_conv_rec]);
    assert_eq!(no_conv.len(), 1);
    assert!(!leaks(&no_conv[0], &p), "no-convention row leaked");

    let skipped = not_assessed_findings("acme/app", &[("ARCH-Z-1".to_string(), p.text.clone())]);
    assert_eq!(skipped.len(), 1);
    assert!(!leaks(&skipped[0], &p), "not-assessed row leaked");
}

fn raw_finding(p: &Planted, line: usize) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: ".env".to_string(),
        line,
        rule_id: "SEC-NO-VENDOR-TOKEN-1".to_string(),
        severity: "high".to_string(),
        snippet: p.text.clone(),
        detail: format!("holds {}", p.value),
        fix_specific: Some(format!("Replace {} with an env var.", p.value)),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

#[test]
fn merge_location_group_output_is_redacted_when_a_member_was_not() {
    // Belt and braces: both members bypass construction-time redaction (as a carried-forward
    // cache row from before the fix would), and the merged row still comes out clean.
    let p = secret();
    let files = vec![(".env".to_string(), format!("# top\n{}\n", p.text))];
    let mut other = raw_finding(&p, 2);
    other.rule_id = "SEC-NO-HARDCODED-SECRETS-1".to_string();
    let merged = merge_by_location(vec![raw_finding(&p, 2), other], &files);
    assert!(!merged.is_empty());
    for f in &merged {
        assert!(!leaks(f, &p), "merged row leaked");
    }
}

#[test]
fn merge_semantic_group_and_structural_group_outputs_are_redacted() {
    let p = secret();
    let mut a = raw_finding(&p, 2);
    a.category = Some("arch-conformance".to_string());
    a.confidence = Some("needs-review".to_string());
    a.severity = "low".to_string();
    let mut b = a.clone();
    b.line = 9;
    let grouped = group_structural_needs_review(vec![a, b]);
    assert_eq!(
        grouped.len(),
        1,
        "two needs-review structural rows fold into one"
    );
    assert!(!leaks(&grouped[0], &p), "structural group row leaked");

    let files = vec![(".env".to_string(), format!("x\n{}\n", p.text))];
    let merged = merge_semantic_groups(vec![raw_finding(&p, 2), raw_finding(&p, 3)], &files);
    for f in &merged {
        assert!(!leaks(f, &p), "semantic group row leaked");
    }
}

#[test]
fn needs_review_reason_appended_to_detail_is_redacted() {
    let p = secret();
    let findings = vec![Finding {
        detail: "A finding.".to_string(),
        ..raw_finding(&p, 3)
    }];
    let raw = serde_json::json!({"verdicts": [{
        "index": 0, "confidence": "low",
        "reason": format!("the key {} may be a test value", p.value),
    }]})
    .to_string();
    let out = apply_verdicts(&raw, findings);
    assert_eq!(out.len(), 1);
    assert!(
        out[0].detail.contains("[needs review:"),
        "{}",
        out[0].detail
    );
    assert!(!out[0].detail.contains(&p.value));
    assert!(detect_secrets(&out[0].detail).is_empty());
}

/// Replies with one canned body to every call and records every request.
struct Stub {
    text: String,
    seen: std::sync::Mutex<Vec<LlmRequest>>,
}

impl Stub {
    fn new(text: &str) -> Self {
        Self {
            text: text.to_string(),
            seen: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Every system prompt and user prompt sent, concatenated.
    fn everything_sent(&self) -> String {
        self.seen
            .lock()
            .expect("lock")
            .iter()
            .map(|r| format!("{}\n{}", r.system.clone().unwrap_or_default(), r.prompt))
            .collect::<Vec<_>>()
            .join("\n=====\n")
    }
}

#[async_trait::async_trait]
impl LlmPort for Stub {
    async fn complete(&self, req: LlmRequest) -> anyhow::Result<LlmResponse> {
        self.seen.lock().expect("lock").push(req);
        Ok(LlmResponse {
            text: self.text.clone(),
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
        req: LlmRequest,
        on_delta: &mut (dyn for<'a> FnMut(&'a str) + Send),
    ) -> anyhow::Result<LlmResponse> {
        let resp = self.complete(req).await?;
        on_delta(&resp.text);
        Ok(resp)
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[tokio::test]
async fn fix_specific_assignment_redacts_model_text_before_the_self_check() {
    let p = secret();
    // The finding was redacted at construction; the MODEL's fix repeats the raw value anyway.
    let mut finding = raw_finding(&p, 3);
    crate::onboard::redact::redact_finding(&mut finding);
    finding.fix_specific = None;
    let reply = serde_json::json!({"fixes": [{
        "index": 0,
        "fix": format!("Move the value {} on line 3 of `.env` into your secrets manager.", p.value),
    }]})
    .to_string();
    let stub = Stub::new(&reply);
    let files = vec![(".env".to_string(), format!("# top\n\n{}\n", p.text))];
    let (out, _) =
        generate_fix_specifics(&stub, "acme/app", vec![finding], &files, None, None, None).await;
    assert_eq!(out.len(), 1);
    let fix = out[0].fix_specific.as_deref().expect("a fix was assigned");
    assert!(!fix.contains(&p.value), "fix leaked: {fix}");
    assert!(
        fix.contains("sb_secret_…"),
        "the redacted form stands in: {fix}"
    );
}

#[tokio::test]
async fn fix_specific_prompt_carries_no_secret_value_and_forbids_repeating_one() {
    let p = secret();
    let prompt = fix_specific_system_prompt();
    assert!(prompt.contains("NEVER repeat a credential value"));
    assert!(prompt.contains("deliberate redaction"));

    let mut finding = raw_finding(&p, 3);
    finding.fix_specific = None;
    // The finding is NOT pre-redacted here: the prompt builder must still never send the value
    // because the file set it reads is the model-facing (redacted) one.
    let files = crate::onboard::redact::files_for_model_with(
        &[(".env".to_string(), format!("# top\n\n{}\n", p.text))],
        true,
    );
    crate::onboard::redact::redact_finding(&mut finding);
    let stub = Stub::new(r#"{"fixes":[]}"#);
    let _ =
        generate_fix_specifics(&stub, "acme/app", vec![finding], &files, None, None, None).await;
    let sent = stub.everything_sent();
    assert!(
        !sent.contains(&p.value),
        "the fix-specific prompt carried the value"
    );
    assert!(
        sent.contains("sb_secret_…"),
        "the redacted form is what the model sees"
    );
}
