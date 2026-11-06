//! Nothing in grouping is quadratic: a synthetic 30,000-row report goes through the collapse,
//! the grouping pass, the tree assembly and the final keying in debug-build time.

use std::time::Instant;

use super::*;
use crate::onboard::Finding;

const ROWS: usize = 30_000;

/// 30,000 rows over 1,400 ordinary files (about 20 each), plus one 2,000-row generated file (the
/// large-file path), over 40 rules (a quarter of them model-named), with captured objects shared
/// across files (long lists in the inverted index) and every 50th row a duplicate of the one
/// before it (so the collapse has real work).
fn synthetic_report(n: usize) -> Vec<Finding> {
    let mut rows: Vec<Finding> = Vec::with_capacity(n);
    let big = n / 15;
    for i in 0..n {
        let (path, line) = if i < big {
            ("src/generated/big.rs".to_string(), 1 + i * 3)
        } else {
            let k = i - big;
            (
                format!("src/mod{}/file{}.rs", (k / 20) % 40, k / 20),
                1 + (k % 20) * 7,
            )
        };
        let rule = i % 40;
        let rule_id = if rule % 4 == 3 {
            format!("AI-MODEL-NAMED-{rule}")
        } else {
            format!("ARCH-SYNTH-RULE-{rule}-1")
        };
        let mut f = Finding {
            repo: format!("acme/svc{}", i % 3),
            path,
            line,
            rule_id,
            severity: ["low", "medium", "high"][i % 3].to_string(),
            snippet: format!("call_{i}(arg_{})", i % 97),
            detail: format!("synthetic finding number {i} about handler {}", i % 211),
            category: Some(
                [
                    "authorization",
                    "injection",
                    "arch-conformance",
                    "performance",
                ][i % 4]
                    .to_string(),
            ),
            ..Finding::default()
        };
        f.captures
            .insert("table".to_string(), format!("table_{}", i % 50));
        if i % 50 == 49 {
            // A duplicate of the previous row at the same place under the same rule.
            let prev: &Finding = &rows[i - 1];
            f.repo = prev.repo.clone();
            f.path = prev.path.clone();
            f.line = prev.line;
            f.rule_id = prev.rule_id.clone();
            f.snippet = prev.snippet.clone();
        }
        rows.push(f);
    }
    rows
}

fn run(n: usize) -> std::time::Duration {
    let rows = synthetic_report(n);
    assert_eq!(rows.len(), n);
    let tools = ToolMap::builtin();

    let t = Instant::now();
    let collapsed = collapse_identical(rows, &tools);
    let collapse_time = t.elapsed();
    assert_eq!(
        collapsed.rows.len() + collapsed.collapsed.len(),
        n,
        "rows_in == rows_out + collapsed"
    );
    assert!(!collapsed.collapsed.is_empty(), "the duplicates collapse");

    // Group per repo, as the scan does.
    let t = Instant::now();
    let mut by_repo: std::collections::BTreeMap<String, Vec<Finding>> = Default::default();
    for f in collapsed.rows {
        by_repo.entry(f.repo.clone()).or_default().push(f);
    }
    let mut alloc = SubgroupAllocator::new();
    let mut grouped: Vec<Finding> = Vec::new();
    let mut sub_nodes = Vec::new();
    for (_repo, repo_rows) in by_repo {
        let n = repo_rows.len();
        let (g, nodes) = group_semantic(repo_rows, &[], &tools, &mut alloc);
        assert_eq!(g.len(), n, "grouping returns every row");
        grouped.extend(g);
        sub_nodes.extend(nodes);
    }
    let group_time = t.elapsed();

    let t = Instant::now();
    let mut report = crate::onboard::build_report(
        vec!["acme/svc0".into(), "acme/svc1".into(), "acme/svc2".into()],
        Vec::new(),
        1,
        grouped,
    );
    report.tree = Some(ReviewTree {
        nodes: sub_nodes,
        ..ReviewTree::default()
    });
    report.attach_tree();
    report.assign_site_keys();
    let tree_time = t.elapsed();

    let tree = report.tree.as_ref().unwrap();
    // Full coverage and uniqueness: the union of every node's members is exactly the row keys,
    // no key is in two nodes and none in zero.
    super::tree_tests::assert_tree_covers_rows(&report.findings, tree);
    assert_eq!(
        report.findings.len(),
        n - n / 50,
        "rows_out == rows_in - collapsed"
    );
    assert!(report.findings.iter().all(|f| f.group_path.len() == 3));
    let total = collapse_time + group_time + tree_time;
    eprintln!(
        "{n}-row scale: collapse {collapse_time:?}, group {group_time:?}, tree+keys {tree_time:?}, total {total:?}; \
         {} rows out, {} collapsed, {} nodes",
        report.findings.len(),
        collapsed.collapsed.len(),
        tree.nodes.len()
    );
    total
}

#[test]
fn thirty_thousand_rows_collapse_group_and_build_a_tree_in_reasonable_time() {
    let half = run(ROWS / 2);
    let full = run(ROWS);
    assert!(
        full.as_secs() < 120,
        "30,000 rows must group in reasonable debug time, took {full:?}"
    );
    // Doubling the rows must not much more than double the time: nothing is quadratic.
    assert!(
        full.as_secs_f64() < half.as_secs_f64() * 3.5 + 1.0,
        "time grew faster than linearly: 15,000 rows took {half:?}, 30,000 took {full:?}"
    );
}
