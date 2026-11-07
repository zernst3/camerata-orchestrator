//! The engine's own notes are never a headline a client reads: a held row keeps a plain headline
//! and says why it is waiting in a field of its own, the evidence gate's row reads plainly, and a
//! rule with no title is named in words and never by its id.

use std::collections::HashMap;

use super::*;
use crate::client_text::{banned_in, BANNED_HEADLINE_PREFIXES};

#[test]
fn a_rule_with_no_title_is_named_in_words_never_by_its_id() {
    let f = finding("AI-PUBLIC-STORAGE-BUCKET", "a.ts", 1, "high");
    let title = title_for_finding(&f, None);
    assert_eq!(title, "Public storage bucket");
    assert!(!BANNED_HEADLINE_PREFIXES
        .iter()
        .any(|p| title.starts_with(p)));
}

#[test]
fn a_held_row_keeps_a_plain_headline_and_says_why_it_is_waiting_in_its_own_field() {
    let f = finding("AI-CUSTOM-ARCH-RULE-1", "a.rs", 1, "medium");
    let report = report_with(vec![f], vec![]);
    let json = build_report_json(
        &crate::report_export::ReviewedReport::unreviewed(&report.clone().seal(), &HashMap::new()),
        None,
        &empty_opts(),
    );
    let held = &json.matrix.informational[0];
    assert!(
        crate::client_text::banned_headline_prefix(&held.headline).is_none(),
        "the headline stays plain: {}",
        held.headline
    );
    assert!(
        !held.headline.to_lowercase().contains("uncited"),
        "{}",
        held.headline
    );
    assert_eq!(
        held.why_waiting.as_deref(),
        Some(
            "No outside standard supports this problem yet, so a reviewer must rule on it \
             before you act."
        )
    );
    // The same row where the curated groups list it carries the same words.
    let site_reasons: Vec<_> = json
        .held_for_review_findings
        .iter()
        .flat_map(|g| g.sites.iter())
        .map(|s| s.why_waiting.clone())
        .collect();
    assert!(site_reasons.iter().all(Option::is_some), "{site_reasons:?}");
}

#[test]
fn each_waiting_reason_is_plain_and_ends_in_the_call_to_the_reviewer() {
    use crate::report_export::waiting_reason;
    let reasons = [
        waiting_reason(true, false, false),
        waiting_reason(false, true, false),
        waiting_reason(false, false, true),
    ];
    for reason in reasons {
        let reason = reason.expect("a reason");
        assert!(
            reason.ends_with("a reviewer must rule on it before you act."),
            "{reason}"
        );
        assert!(banned_in(reason).is_empty(), "{reason}");
        assert!(crate::client_text::banned_headline_prefix(reason).is_none());
    }
    assert_eq!(waiting_reason(false, false, false), None);
    // The citation gate decides first, as it decides the bucket first.
    assert_eq!(
        waiting_reason(true, true, true),
        waiting_reason(true, false, false)
    );
}

#[test]
fn the_evidence_gate_row_reads_plainly_and_is_held_for_a_decision() {
    let rule = "ARCH-EXPLICIT-TX-1";
    let mut report = report_with(Vec::new(), vec![rule]);
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    ledger.record_rule(
        "",
        rule,
        crate::scan_ledger::RuleTier::Semantic,
        true,
        None,
        10,
        0,
    );
    report.ledger = ledger;
    report.recommendations.insert(
        rule.to_string(),
        crate::ai_audit::RuleRecommendation {
            repo: String::new(),
            rule_id: rule.to_string(),
            recommended_option_id: "some-option".to_string(),
            recommendation_reasoning: "the model's pick".to_string(),
            hallucinated: false,
            operator_chosen: false,
            evidence: Some(
                "src/orders/service.rs:140 - three sequential writes with no transaction"
                    .to_string(),
            ),
            applicable: true,
            ..Default::default()
        },
    );
    apply_evidence_gate(&mut report);
    let row = report
        .findings
        .iter()
        .find(|f| f.rule_id == rule)
        .expect("the gate adds a row");
    assert!(row.needs_review, "the row waits for a decision");
    let (headline, detail) = client_headline_and_detail(row, None, "Fallback", None);
    for text in [&headline, &detail] {
        assert!(
            crate::client_text::banned_headline_prefix(text).is_none(),
            "{text}"
        );
        assert!(!text.contains("own evaluation evidence"), "{text}");
    }
    assert!(
        headline.starts_with("This rule found no violation"),
        "{headline}"
    );
    assert!(detail.contains("A reviewer must decide"), "{detail}");
    // The evidence itself is kept for the reviewer, quoted in the detail and in the snippet.
    assert!(
        detail.contains("three sequential writes with no transaction"),
        "{detail}"
    );
    assert!(
        row.snippet.contains("three sequential writes"),
        "{}",
        row.snippet
    );
}
