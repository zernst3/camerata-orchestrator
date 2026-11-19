//! WO9, design 3B item 1 and amendment A14: the PDF as summary, escalations and patterns, the
//! reshaped PDF above 30,000 sites, and the escalation sentence derived from the reviewed tree.

use camerata_api_types::review::{EscalationCandidate, EscalationReason, EscalationState};
use serde_json::Value;

use super::support::*;
use crate::onboard::MergedLocation;
use crate::report_export::{apply_overlay, build_report_json, render_data_json};

fn escalated_report() -> crate::onboard::SealedReport {
    let mut report = mixed_sealed().into_inner();
    // Two sites the group reviewer put in the architect's queue.
    let keys: Vec<String> = [0usize, 3]
        .iter()
        .map(|&i| report.findings[i].site_key.clone())
        .collect();
    for (n, key) in keys.iter().enumerate() {
        let f = report
            .findings
            .iter_mut()
            .find(|f| &f.site_key == key)
            .unwrap();
        f.review.escalation_state = EscalationState::Escalated;
        f.review.escalation_reason = Some(EscalationReason::RuleMisfit);
        f.review.escalation_why = "SECRET-WHY-ESCALATION".to_string();
        f.review.stage2_rank = Some(n as u32 + 1);
    }
    let tree = report.tree.as_mut().expect("a tree");
    tree.budget = camerata_api_types::review::EscalationBudget::compute(1.0, 20, 400);
    tree.candidates = keys
        .iter()
        .enumerate()
        .map(|(n, key)| EscalationCandidate {
            key: key.clone(),
            node_id: "rule:X".to_string(),
            reason: EscalationReason::RuleMisfit,
            severity: "high".to_string(),
            stage2_rank: n as u32 + 1,
            global_rank: n as u32 + 1,
            stage2_severity: None,
            slot: 0,
            escalated: true,
            over_budget_reason: None,
            why: "SECRET-WHY-ESCALATION".to_string(),
        })
        .collect();
    tree.budget.escalated = 2;
    tree.budget.candidates = 2;
    // The ledger recorded three at stage 2; the tree, after the seal, holds two.
    report
        .ledger
        .record_escalation(crate::scan_ledger::EscalationBlock {
            model: Some("a-model".to_string()),
            rate_per_kloc: 1.0,
            floor: 20,
            counted_lines: 400,
            budget: 20,
            candidates: 3,
            escalated: 3,
            over_budget: 0,
            total_sites: 6,
            share_escalated: 0.5,
            ..Default::default()
        });
    report.seal()
}

#[test]
fn the_ledger_escalation_sentence_is_derived_from_the_reviewed_tree() {
    let sealed = escalated_report();
    // What stage 2 recorded still says three; the reviewed tree says two.
    assert_eq!(
        sealed.ledger.escalation().unwrap().sentence(),
        "Escalated 3 of a budget of 20 (none over)"
    );
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let json = build_report_json(&reviewed, None, &client_options());
    let value = serde_json::to_value(&json).unwrap();
    assert_eq!(
        value["escalations"]["sentence"], "Escalated 2 of a budget of 20 (none over)",
        "{}",
        value["escalations"]
    );
    assert_eq!(value["escalations"]["items"].as_array().unwrap().len(), 2);
    // The group reviewer's words are not in the PDF's data.
    let data = render_data_json(&json).unwrap();
    assert!(!data.contains("SECRET-WHY-ESCALATION"));
    assert!(!data.contains("rule_misfit"));
}

#[test]
fn escalations_carry_their_decisions() {
    let sealed = escalated_report();
    let key = key_at(&sealed, "src/a.rs");
    let reviewed = apply_overlay(
        &sealed,
        &[decision(
            "dec-000001",
            &key,
            camerata_api_types::decisions::DecisionState::Confirmed,
            "",
        )],
        &[],
        &[],
    );
    let json = build_report_json(&reviewed, None, &client_options());
    let value = serde_json::to_value(&json).unwrap();
    let items = value["escalations"]["items"].as_array().unwrap();
    let a = items.iter().find(|i| i["path"] == "src/a.rs").unwrap();
    assert!(
        a["decision"]
            .as_str()
            .unwrap()
            .starts_with("Confirmed by the reviewer"),
        "{a}"
    );
    assert_eq!(a["review_level"], "personally_reviewed");
    let d = items.iter().find(|i| i["path"] == "src/d.rs").unwrap();
    assert!(d["decision"].as_str().unwrap().starts_with("Open"), "{d}");
}

/// One finding that stands for `sites` places: its own plus `sites - 1` absorbed ones.
fn report_of_sites(sites: usize) -> crate::onboard::SealedReport {
    let mut report = crate::release_gate::test_scan_report();
    let mut f = sql_row("src/primary.rs", 7);
    f.also_locations = (0..sites - 1)
        .map(|i| {
            MergedLocation::site(
                "acme/portal".to_string(),
                format!("src/generated/module_{i:05}.rs"),
                i + 1,
                "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
                format!("let q = format!(\"SELECT {i}\");"),
                false,
            )
        })
        .collect();
    report.findings = vec![f];
    report.seal()
}

#[test]
fn the_pdf_data_is_a_summary_at_every_size() {
    // The shape is the same at 30,000 places and at 30,001: no switch, no per-place list.
    for places in [30_000usize, 30_001] {
        let reviewed = apply_overlay(&report_of_sites(places), &[], &[], &[]);
        let built = build_report_json(&reviewed, None, &client_options());
        let data = render_data_json(&built).unwrap();
        let json: Value = serde_json::from_str(&data).unwrap();
        assert!(json.get("reduced").is_none(), "{places}: no size switch");
        assert!(json.get("reduced_matrix").is_none(), "{places}");
        assert_eq!(json["site_total"], places);
        // The lists the template does not read are not in the data it parses.
        for unread in ["curated_findings", "held_for_review_findings", "matrix"] {
            assert!(
                json.get(unread).is_none(),
                "{places}: {unread} is in the data"
            );
        }
        // The report itself still holds them, for the release gate's dataset checks.
        let in_memory = serde_json::to_value(&built).unwrap();
        assert!(!in_memory["curated_findings"].as_array().unwrap().is_empty());
        let patterns = json["sub_group_patterns"].as_array().unwrap();
        assert_eq!(patterns.len(), 1);
        assert_eq!(patterns[0]["site_count"], places);
        assert!(
            patterns[0]["pointer"]
                .as_str()
                .unwrap()
                .starts_with(&format!(
                    "{places} places, see the All problems sheet, filter Fix id = "
                )),
            "{}",
            patterns[0]["pointer"]
        );
        // No per-place text reaches the PDF's data: the data is small whatever the place count.
        assert!(
            !data.contains("module_00001.rs"),
            "a place is in the PDF data"
        );
        assert!(data.len() < 200_000, "{places}: {} bytes", data.len());
    }
}

fn futures_lite_block_on<T>(fut: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime")
        .block_on(fut)
}

#[test]
fn a_package_of_thirty_thousand_places_clears_the_release_gate() {
    let over = apply_overlay(&report_of_sites(30_001), &[], &[], &[]);
    let entries = released_entries(&over, &client_options());
    assert!(entries.contains_key("findings.json"));
    let _ = Value::Null;
}

#[test]
fn the_compiled_pdf_prints_the_escalations_and_the_patterns() {
    if !camerata_test_env::typst_on_path() {
        eprintln!("skipping the_compiled_pdf_prints_the_escalations_and_the_patterns: no typst");
        return;
    }
    let mut report = escalated_report().into_inner();
    report.findings[0].fix_specific = Some("Bind the parameters with the driver.".to_string());
    let sealed = report.seal();
    let reviewed = apply_overlay(&sealed, &[], &[], &[]);
    let json = build_report_json(&reviewed, None, &client_options());
    let data = render_data_json(&json).unwrap();
    let pdf = futures_lite_block_on(crate::report_export::compile_pdf_from_data(&data))
        .expect("the PDF compiles");
    let text = pdf_extract::extract_text_from_mem(&pdf).expect("the PDF's text extracts");
    let flat: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    for want in [
        "The most serious problems",
        "What to do: Bind the parameters with the driver.",
        "The other fixes",
    ] {
        assert!(
            flat.contains(want),
            "the PDF does not print {want:?}: {flat}"
        );
    }
    for gone in [
        "Escalated 2 of a budget of 20",
        "Sites reviewed in person",
        "What the findings have in common",
        "filter Fix id = ",
    ] {
        assert!(
            !flat.contains(gone),
            "the PDF still prints {gone:?}: {flat}"
        );
    }
    assert!(!flat.contains("SECRET-WHY-ESCALATION"));
}

#[test]
fn ledger_states_budget_escalated_and_over_budget() {
    // Design 4.8 item 12, at export: three escalated against a budget of one, two of them past
    // it, all at critical severity.
    let mut report = mixed_sealed().into_inner();
    let keys: Vec<String> = report
        .findings
        .iter()
        .take(3)
        .map(|f| f.site_key.clone())
        .collect();
    let tree = report.tree.as_mut().expect("a tree");
    tree.budget = camerata_api_types::review::EscalationBudget::compute(0.0, 1, 400);
    tree.candidates = keys
        .iter()
        .enumerate()
        .map(|(n, key)| EscalationCandidate {
            key: key.clone(),
            node_id: "rule:X".to_string(),
            reason: EscalationReason::UnauthenticatedReach,
            severity: "critical".to_string(),
            stage2_rank: 1,
            global_rank: n as u32 + 1,
            stage2_severity: None,
            slot: 0,
            escalated: true,
            over_budget_reason: (n > 0).then_some(EscalationReason::UnauthenticatedReach),
            why: String::new(),
        })
        .collect();
    tree.budget.escalated = 3;
    tree.budget.over_budget = 2;
    tree.budget.candidates = 3;
    report
        .ledger
        .record_escalation(crate::scan_ledger::EscalationBlock {
            budget: 1,
            escalated: 1,
            ..Default::default()
        });
    let reviewed = apply_overlay(&report.seal(), &[], &[], &[]);
    let block = reviewed
        .report()
        .ledger
        .escalation()
        .expect("the reviewed ledger states the escalation");
    assert_eq!(
        (block.budget, block.escalated, block.over_budget),
        (1, 3, 2)
    );
    let sentence = block.sentence();
    assert_eq!(
        sentence,
        "Escalated 3 of a budget of 1 (2 over, all unauthenticated reach at critical)"
    );
    let value =
        serde_json::to_value(build_report_json(&reviewed, None, &client_options())).unwrap();
    assert_eq!(value["escalations"]["sentence"], sentence);
}
