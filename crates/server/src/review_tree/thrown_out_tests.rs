//! A12: the thrown-out node. `build_tree` takes the rows the engine threw out as a second input
//! and puts each in a `cat:<category>/thrown-out` node with a sub-group per discard reason, so
//! every row of a report is in exactly one node, counting the discarded rows.

use std::collections::BTreeMap;

use super::*;
use crate::engine_discard;
use crate::inspections::backfill_site_keys;
use crate::onboard::Finding;

fn row(rule_id: &str, path: &str, line: usize, snippet: &str) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule_id.to_string(),
        severity: "medium".to_string(),
        snippet: snippet.to_string(),
        detail: format!("detail for {rule_id}"),
        ..Finding::default()
    }
}

fn thrown(rule_id: &str, path: &str, line: usize, reason: &str) -> Finding {
    let mut f = row(rule_id, path, line, &format!("thrown {path}:{line}"));
    engine_discard::mark(&mut f, reason);
    f
}

/// Kept rows of two rules and discarded rows of those rules plus one rule that has no kept row.
fn fixture() -> (Vec<Finding>, Vec<Finding>) {
    let kept = vec![
        row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 3, "query(a)"),
        row("SEC-NO-RAW-SQL-CONCAT-1", "src/b.rs", 9, "query(b)"),
        row("SEC-NO-HARDCODED-SECRETS-1", "src/c.rs", 4, "key = \"x\""),
    ];
    let thrown = vec![
        thrown(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/d.rs",
            1,
            engine_discard::DISCARD_VERDICT,
        ),
        thrown(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/e.rs",
            2,
            engine_discard::DISCARD_VERDICT,
        ),
        thrown(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/f.rs",
            3,
            engine_discard::RULE_NOT_APPLICABLE,
        ),
        thrown(
            "SEC-RLS-ENABLED-1",
            "db/m.sql",
            7,
            engine_discard::NO_CONVENTION,
        ),
    ];
    (kept, thrown)
}

fn build(kept: &mut Vec<Finding>, discarded: &mut Vec<Finding>) -> ReviewTree {
    let tools = ToolMap::builtin();
    backfill_site_keys(kept);
    backfill_site_keys(discarded);
    build_tree(kept, discarded, None, &tools, "run-1")
}

#[test]
fn every_row_is_in_exactly_one_node_counting_discarded_rows() {
    let (mut kept, mut discarded) = fixture();
    let tree = build(&mut kept, &mut discarded);

    let mut owner: BTreeMap<&str, &str> = BTreeMap::new();
    for node in &tree.nodes {
        for key in &node.members {
            assert!(
                owner.insert(key.as_str(), node.id.as_str()).is_none(),
                "{key:?} is in two nodes"
            );
        }
    }
    let mut all: Vec<&str> = kept
        .iter()
        .chain(discarded.iter())
        .map(|f| f.site_key.as_str())
        .collect();
    all.sort_unstable();
    let mut members: Vec<&str> = owner.keys().copied().collect();
    members.sort_unstable();
    assert_eq!(members, all, "kept and discarded rows are all members");
    assert_eq!(all.len(), kept.len() + discarded.len());
    for f in kept.iter().chain(discarded.iter()) {
        assert_eq!(
            owner.get(f.site_key.as_str()).copied(),
            f.group_path.get(2).map(String::as_str),
            "{} sits in the sub-group its group_path names",
            f.site_key
        );
    }
}

#[test]
fn there_is_one_thrown_out_node_per_category_with_a_sub_group_per_reason() {
    let (mut kept, mut discarded) = fixture();
    let tree = build(&mut kept, &mut discarded);

    let thrown_out: Vec<&TreeNode> = tree
        .nodes
        .iter()
        .filter(|n| n.id.ends_with("/thrown-out"))
        .collect();
    assert!(
        discarded.iter().all(|f| f.group_path.len() == 3),
        "build_tree writes a place on every discarded row"
    );
    let sql_category = kept[0].group_path[0].clone();
    let rls_category = discarded[3].group_path[0].clone();
    assert_ne!(
        sql_category, rls_category,
        "the fixture spans two categories"
    );
    let mut ids: Vec<&str> = thrown_out.iter().map(|n| n.id.as_str()).collect();
    ids.sort_unstable();
    let mut want = vec![
        format!("{sql_category}/thrown-out"),
        format!("{rls_category}/thrown-out"),
    ];
    want.sort();
    assert_eq!(ids, want.iter().map(String::as_str).collect::<Vec<_>>());

    let sql_node = tree
        .node(&format!("{sql_category}/thrown-out"))
        .expect("the sql category has a thrown-out node");
    assert_eq!(sql_node.parent.as_deref(), Some(sql_category.as_str()));
    let subs: Vec<&TreeNode> = tree
        .nodes
        .iter()
        .filter(|n| n.parent.as_deref() == Some(sql_node.id.as_str()))
        .collect();
    assert_eq!(subs.len(), 2, "one sub-group per reason: {subs:#?}");
    let mut by_reason: BTreeMap<&str, usize> = BTreeMap::new();
    for s in &subs {
        assert_eq!(s.level, NodeLevel::Subgroup);
        by_reason.insert(s.cause.as_deref().unwrap_or(""), s.members.len());
    }
    assert_eq!(by_reason[engine_discard::DISCARD_VERDICT], 2);
    assert_eq!(by_reason[engine_discard::RULE_NOT_APPLICABLE], 1);
    // The category that holds only discarded rows still has its category node as the parent.
    assert!(tree.node(&rls_category).is_some());
}

#[test]
fn a_batch_decision_on_a_category_never_reaches_a_thrown_out_row() {
    let (mut kept, mut discarded) = fixture();
    let tree = build(&mut kept, &mut discarded);
    let sql_category = kept[0].group_path[0].clone();
    let covered = tree.batch_members(&sql_category);
    for f in &discarded {
        assert!(
            !covered.contains(&f.site_key),
            "{} is thrown out and a category decision must not write to it",
            f.site_key
        );
    }
    for f in kept.iter().filter(|f| f.group_path[0] == sql_category) {
        assert!(covered.contains(&f.site_key));
    }
    // The thrown-out node itself can still be targeted on purpose.
    let node = format!("{sql_category}/thrown-out");
    let own = tree.batch_members(&node);
    assert_eq!(own.len(), 3);
}

#[test]
fn a_report_with_nothing_thrown_out_has_no_thrown_out_node() {
    let (mut kept, mut discarded) = fixture();
    discarded.clear();
    let tree = build(&mut kept, &mut discarded);
    assert!(tree.nodes.iter().all(|n| !n.id.contains("thrown-out")));
}

#[test]
fn attach_tree_on_a_report_includes_the_discarded_rows() {
    let (kept, discarded) = fixture();
    let mut report = crate::onboard::ScanReport::gated(&[]);
    report.findings = kept;
    report.discarded = discarded;
    report.assign_site_keys();
    let tree = report.tree.as_ref().expect("a tree");
    let thrown_keys: Vec<&str> = report
        .discarded
        .iter()
        .map(|f| f.site_key.as_str())
        .collect();
    for key in thrown_keys {
        assert!(
            tree.nodes
                .iter()
                .any(|n| n.members.iter().any(|m| m == key)),
            "{key} is in a node"
        );
    }
}
