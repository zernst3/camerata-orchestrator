//! A confirmed cluster is one row per site (design amendment A11): each copy is its own row with
//! its own place, quote and site key, so the grouping can place it, the evidence check can look at
//! it and a reviewer can decide it.

use std::collections::BTreeSet;

use super::*;
use crate::review_tree::{build_tree, group_semantic, SubgroupAllocator, ToolMap};
use crate::scan_ledger::ScanLedger;

const RULE: CloneRule = CloneRule::SingleSourceLogic;

/// The same discount written in four files.
fn four_copies() -> Vec<(String, String)> {
    files(&[
        ("src/cart/total.ts", &discount("applyDiscount", "0.2")),
        ("src/checkout/summary.ts", &discount("quoteTotal", "0.15")),
        ("src/orders/price.ts", &discount("priceOrder", "0.2")),
        ("src/invoices/amount.ts", &discount("invoiceAmount", "0.1")),
    ])
}

async fn four_site_outcome() -> (Vec<(String, String)>, JudgeOutcome) {
    let all = four_copies();
    let stub = StubJudge::answering("same_decision", "the premium discount calculation");
    let outcome = judge(Some(&stub), &all, set_for(&all, &[RULE])).await;
    (all, outcome)
}

#[tokio::test]
async fn a_cluster_of_four_sites_is_four_rows_with_a_place_each() {
    let (all, outcome) = four_site_outcome().await;
    assert_eq!(outcome.findings.len(), 4, "{:#?}", outcome.findings);
    assert_eq!(outcome.counts[&RULE].confirmed, 1, "one cluster was judged");

    let paths: BTreeSet<&str> = outcome.findings.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(
        paths,
        BTreeSet::from([
            "src/cart/total.ts",
            "src/checkout/summary.ts",
            "src/orders/price.ts",
            "src/invoices/amount.ts",
        ])
    );
    for f in &outcome.findings {
        assert!(f.also_locations.is_empty(), "{f:#?}");
        assert_eq!(f.rule_id, RULE.id());
        // Its own quote: the first line of its own copy.
        let content = &all.iter().find(|(p, _)| *p == f.path).unwrap().1;
        assert_eq!(
            Some(f.snippet.as_str()),
            content.lines().nth(f.line - 1),
            "{} quotes its own line",
            f.path
        );
    }
    // The shared text is the same on every row and names every place.
    let first = &outcome.findings[0];
    for f in &outcome.findings {
        assert_eq!(f.detail, first.detail);
        assert_eq!(f.severity, first.severity);
        assert_eq!(f.captures, first.captures);
    }
    for (path, _) in &all {
        assert!(first.detail.contains(path.as_str()), "{}", first.detail);
    }
}

#[tokio::test]
async fn the_four_rows_land_in_one_sub_group_of_the_tree() {
    let (all, outcome) = four_site_outcome().await;
    let tools = ToolMap::builtin();
    let mut alloc = SubgroupAllocator::new();
    let (mut rows, sub_nodes) = group_semantic(outcome.findings.clone(), &all, &tools, &mut alloc);
    assert_eq!(rows.len(), 4, "grouping returns every row");
    let tree = build_tree(
        &mut rows,
        Some(crate::review_tree::ReviewTree {
            nodes: sub_nodes,
            ..Default::default()
        }),
        &tools,
        "run-1",
    );
    let subs: BTreeSet<&str> = rows.iter().map(|f| f.group_path[2].as_str()).collect();
    assert_eq!(subs.len(), 1, "one sub-group: {subs:?}");
    let sub = tree
        .nodes
        .iter()
        .find(|n| n.id == *subs.iter().next().unwrap())
        .expect("the sub-group is in the tree");
    assert_eq!(sub.members.len(), 4, "{sub:#?}");
    let keys: BTreeSet<&str> = rows.iter().map(|f| f.site_key.as_str()).collect();
    assert_eq!(keys.len(), 4, "each row has a site key of its own");
    assert!(sub.members.iter().all(|m| keys.contains(m.as_str())));
}

#[tokio::test]
async fn every_row_passes_the_evidence_check() {
    let (all, mut outcome) = four_site_outcome().await;
    let counts = crate::onboard::evidence_check::verify_evidence(&mut outcome.findings, &all, None);
    assert_eq!(
        (counts.checked, counts.verified, counts.unverified),
        (4, 4, 0)
    );
    assert_eq!(
        counts.sites_seen(),
        0,
        "no absorbed sites are left to check"
    );
    assert!(counts.reconciles());
    for f in &outcome.findings {
        assert!(
            !f.captures
                .contains_key(crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE),
            "{f:#?}"
        );
    }
}

#[tokio::test]
async fn the_ledger_counts_the_rows_it_emitted() {
    let (_all, outcome) = four_site_outcome().await;
    let mut ledger = ScanLedger::new();
    record_in_ledger(&mut ledger, "", &outcome);
    let entry = ledger.rule(RULE.id()).expect("recorded");
    assert!(entry.ran);
    assert_eq!(
        entry.findings_emitted,
        outcome.findings.len(),
        "the ledger counts rows, and a cluster is four rows"
    );
    assert_eq!(entry.findings_emitted, 4);
}
