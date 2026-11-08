//! The Fix merge (item #1, design R-4): the place set of a sub-group is its key, containment folds,
//! overlap never merges, and a whole-file place contains its file's lines only at equal or worse
//! severity. Synthetic fixtures only.

use super::*;
use crate::onboard::Finding;

fn node(id: &str) -> TreeNode {
    TreeNode {
        id: id.to_string(),
        level: NodeLevel::Subgroup,
        parent: None,
        label: id.to_string(),
        rule_ids: Vec::new(),
        kind: NodeKind::Bulk,
        cause: None,
        members: Vec::new(),
        outliers: Vec::new(),
        summary: None,
        summary_source: TextSource::Model,
        escalations: Vec::new(),
        flags: Vec::new(),
    }
}

fn row(sub: &str, rule: &str, severity: &str, path: &str, line: usize) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        snippet: format!("{rule} {path}:{line}"),
        group_path: vec!["cat:x".to_string(), format!("rule:{rule}"), sub.to_string()],
        ..Finding::default()
    }
}

fn fixes_of(rows: &[Finding]) -> Vec<FixNode> {
    let mut ids: Vec<String> = rows
        .iter()
        .filter_map(|f| f.group_path.get(2).cloned())
        .collect();
    ids.sort();
    ids.dedup();
    let tree = ReviewTree {
        nodes: ids.iter().map(|i| node(i)).collect(),
        ..ReviewTree::default()
    };
    merge_fixes(&tree, rows)
}

fn members(fixes: &[FixNode]) -> Vec<Vec<String>> {
    fixes.iter().map(|f| f.members.clone()).collect()
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

#[test]
fn equal_place_sets_across_rule_families_are_one_fix() {
    let rows = vec![
        row("sub:A/1", "A", "critical", ".env.production", 6),
        row("sub:B/1", "B", "high", ".env.production", 6),
        row("sub:C/1", "C", "high", ".env.production", 6),
        row("sub:D/1", "D", "medium", "src/other.ts", 9),
    ];
    let fixes = fixes_of(&rows);
    assert_eq!(
        members(&fixes),
        vec![
            names(&["sub:A/1", "sub:B/1", "sub:C/1"]),
            names(&["sub:D/1"])
        ]
    );
    assert_eq!(fixes[0].rules, names(&["A", "B", "C"]));
    assert_eq!(fixes[0].places.len(), 1);
}

#[test]
fn a_contained_sub_group_folds_in_and_names_its_places() {
    // Rule 1 at A and B, rule 2 at A only: one Fix with places A and B.
    let rows = vec![
        row("sub:R1/1", "R1", "high", "a.ts", 1),
        row("sub:R1/1", "R1", "high", "b.ts", 2),
        row("sub:R2/1", "R2", "high", "a.ts", 1),
    ];
    let fixes = fixes_of(&rows);
    assert_eq!(members(&fixes), vec![names(&["sub:R1/1", "sub:R2/1"])]);
    assert_eq!(fixes[0].places.len(), 2);
    assert_eq!(fixes[0].rules, names(&["R1", "R2"]));
    assert!(fixes[0].shares.is_empty());
}

#[test]
fn a_true_partial_overlap_stays_two_fixes_that_cross_reference() {
    // Rule 1 at A and B, rule 2 at A and C: neither contains the other.
    let rows = vec![
        row("sub:R1/1", "R1", "high", "a.ts", 1),
        row("sub:R1/1", "R1", "high", "b.ts", 2),
        row("sub:R2/1", "R2", "high", "a.ts", 1),
        row("sub:R2/1", "R2", "high", "c.ts", 3),
    ];
    let fixes = fixes_of(&rows);
    assert_eq!(
        members(&fixes),
        vec![names(&["sub:R1/1"]), names(&["sub:R2/1"])]
    );
    assert_eq!(
        fixes[0].shares,
        vec![FixShare {
            fix: fixes[1].id.clone(),
            places: 1
        }]
    );
    assert_eq!(
        fixes[1].shares,
        vec![FixShare {
            fix: fixes[0].id.clone(),
            places: 1
        }]
    );
    // Both list the shared place.
    let shared = FixPlace {
        repo: "acme/app".to_string(),
        path: "a.ts".to_string(),
        line: 1,
    };
    assert!(fixes[0].places.contains(&shared) && fixes[1].places.contains(&shared));
}

#[test]
fn a_shared_place_never_chains_fifty_places_into_another_fix() {
    let mut rows: Vec<Finding> = (0..50)
        .map(|i| row("sub:BIG/1", "BIG", "medium", &format!("src/f{i}.ts"), 10))
        .collect();
    // The small sub-group shares exactly one place with the big one and has another of its own.
    rows.push(row("sub:SMALL/1", "SMALL", "medium", "src/f0.ts", 10));
    rows.push(row("sub:SMALL/1", "SMALL", "medium", "src/own.ts", 4));
    let fixes = fixes_of(&rows);
    assert_eq!(
        members(&fixes),
        vec![names(&["sub:BIG/1"]), names(&["sub:SMALL/1"])]
    );
    // A third sub-group at the shared place alone sits in two sets that do not contain each
    // other, so it stays its own Fix and joins neither.
    rows.push(row("sub:THIRD/1", "THIRD", "medium", "src/f0.ts", 10));
    let fixes = fixes_of(&rows);
    assert_eq!(
        members(&fixes),
        vec![
            names(&["sub:BIG/1"]),
            names(&["sub:SMALL/1"]),
            names(&["sub:THIRD/1"])
        ]
    );
}

#[test]
fn a_nested_chain_folds_into_the_largest_and_two_incomparable_supersets_do_not() {
    // A is in B is in C: one Fix.
    let chain = vec![
        row("sub:A/1", "A", "high", "a.ts", 1),
        row("sub:B/1", "B", "high", "a.ts", 1),
        row("sub:B/1", "B", "high", "b.ts", 1),
        row("sub:C/1", "C", "high", "a.ts", 1),
        row("sub:C/1", "C", "high", "b.ts", 1),
        row("sub:C/1", "C", "high", "c.ts", 1),
    ];
    assert_eq!(
        members(&fixes_of(&chain)),
        vec![names(&["sub:A/1", "sub:B/1", "sub:C/1"])]
    );
    // S sits in T1 and in T2, which do not contain each other: all three stay apart.
    let incomparable = vec![
        row("sub:S/1", "S", "high", "a.ts", 1),
        row("sub:T1/1", "T1", "high", "a.ts", 1),
        row("sub:T1/1", "T1", "high", "b.ts", 1),
        row("sub:T2/1", "T2", "high", "a.ts", 1),
        row("sub:T2/1", "T2", "high", "c.ts", 1),
    ];
    assert_eq!(
        members(&fixes_of(&incomparable)),
        vec![
            names(&["sub:S/1"]),
            names(&["sub:T1/1"]),
            names(&["sub:T2/1"])
        ]
    );
}

#[test]
fn a_whole_file_row_absorbs_same_file_lines_only_at_equal_or_worse_severity() {
    // A critical whole-file row absorbs the lines of the same file.
    let absorbed = vec![
        row("sub:FILE/1", "FILE", "critical", ".env.production", 0),
        row("sub:LINE/1", "LINE", "high", ".env.production", 6),
    ];
    assert_eq!(
        members(&fixes_of(&absorbed)),
        vec![names(&["sub:FILE/1", "sub:LINE/1"])]
    );
    // A low whole-file row never absorbs a critical line.
    let kept_apart = vec![
        row("sub:FILE/1", "FILE", "low", ".env.production", 0),
        row("sub:LINE/1", "LINE", "critical", ".env.production", 6),
    ];
    assert_eq!(
        members(&fixes_of(&kept_apart)),
        vec![names(&["sub:FILE/1"]), names(&["sub:LINE/1"])]
    );
    // Equal severity absorbs.
    let equal = vec![
        row("sub:FILE/1", "FILE", "high", "a.ts", 0),
        row("sub:LINE/1", "LINE", "high", "a.ts", 3),
    ];
    assert_eq!(members(&fixes_of(&equal)).len(), 1);
    // Another file is not absorbed.
    let other_file = vec![
        row("sub:FILE/1", "FILE", "critical", "a.ts", 0),
        row("sub:LINE/1", "LINE", "high", "b.ts", 3),
    ];
    assert_eq!(members(&fixes_of(&other_file)).len(), 2);
}

#[test]
fn dependency_sub_groups_never_merge() {
    let dep = |sub: &str, path: &str| {
        let mut f = row(sub, crate::dep_audit::DEP_AUDIT_RULE_ID, "high", path, 0);
        f.snippet = "left-pad".to_string();
        f
    };
    let rows = vec![
        dep("sub:DEP/1", "package-lock.json"),
        dep("sub:DEP/2", "package-lock.json"),
        row("sub:A/1", "A", "high", "package-lock.json", 0),
    ];
    let fixes = fixes_of(&rows);
    assert_eq!(
        members(&fixes),
        vec![
            names(&["sub:A/1"]),
            names(&["sub:DEP/1"]),
            names(&["sub:DEP/2"])
        ]
    );
}

#[test]
fn every_sub_group_is_in_exactly_one_fix_and_the_ids_are_stable() {
    let rows = vec![
        row("sub:A/1", "A", "high", "a.ts", 1),
        row("sub:B/1", "B", "high", "a.ts", 1),
        row("sub:C/1", "C", "low", "z.ts", 9),
    ];
    let first = fixes_of(&rows);
    let mut shuffled = rows.clone();
    shuffled.reverse();
    let second = fixes_of(&shuffled);
    assert_eq!(first, second);
    let all: Vec<&String> = first.iter().flat_map(|f| &f.members).collect();
    assert_eq!(all.len(), 3);
    assert_eq!(first[0].id, fix_id_of(&first[0].members));
    assert_eq!(first[0].id.len(), 12);
}

#[test]
fn the_ledger_block_balances_sub_groups_in_against_fixes_out_plus_folded() {
    let rows = vec![
        row("sub:A/1", "A", "high", "a.ts", 1),
        row("sub:B/1", "B", "high", "a.ts", 1),
        row("sub:C/1", "C", "low", "z.ts", 9),
    ];
    let fixes = fixes_of(&rows);
    let block = FixMergeBlock::of(&fixes, 3);
    assert_eq!(block.stage, FIX_MERGE_STAGE);
    assert_eq!(
        (block.sub_groups_in, block.fixes_out, block.folded.len()),
        (3, 2, 1)
    );
    assert!(block.is_integral());
    assert_eq!(block.folded[0].0, "sub:B/1");
}

#[test]
fn a_decision_can_be_made_at_a_fix_and_covers_the_members_of_every_sub_group_in_it() {
    let mut rows = vec![
        row("sub:A/1", "A", "high", "a.ts", 1),
        row("sub:B/1", "B", "high", "a.ts", 1),
    ];
    crate::inspections::backfill_site_keys(&mut rows);
    let mut tree = ReviewTree {
        nodes: vec![node("sub:A/1"), node("sub:B/1")],
        ..ReviewTree::default()
    };
    tree.nodes[0].members = vec![rows[0].site_key.clone()];
    tree.nodes[1].members = vec![rows[1].site_key.clone()];
    tree.refresh_fixes(&rows);
    let id = tree.fixes[0].node_id();
    assert!(tree.is_decidable(&id));
    assert!(!tree.is_decidable("fix:nothing"));
    let mut expected = vec![rows[0].site_key.clone(), rows[1].site_key.clone()];
    expected.sort();
    assert_eq!(tree.batch_members(&id), expected);
}

#[test]
fn thousands_of_rule_groups_merge_without_visiting_every_pair() {
    // 3,000 rule groups over 600 files, each holding three lines of two files: the merge asks each
    // group only about the groups that hold its first place, so this finishes at once. Every group
    // lands in exactly one Fix and the result does not depend on the order of the rows.
    let mut rows: Vec<Finding> = Vec::new();
    for g in 0..3_000usize {
        let sub = format!("sub:R{g}/0");
        for k in 0..3usize {
            let file = (g * 7 + k * 13) % 600;
            rows.push(row(
                &sub,
                &format!("R{g}"),
                "medium",
                &format!("src/f{file}.ts"),
                10 + k,
            ));
        }
    }
    let first = fixes_of(&rows);
    let in_fixes: usize = first.iter().map(|f| f.members.len()).sum();
    assert_eq!(in_fixes, 3_000);
    let mut reversed = rows.clone();
    reversed.reverse();
    assert_eq!(first, fixes_of(&reversed));
}

#[test]
fn an_incomparable_pair_folds_into_the_eligible_whole_file_container() {
    // R-4a: C sits in B by place and in FILE by the whole-file ruling, and B and FILE do not
    // contain each other. Exactly one of the two is a whole-file container of C's file at a
    // severity that allows it, so C folds into FILE; B keeps its own places and stays apart.
    let rows = vec![
        row("sub:FILE/1", "FILE", "critical", ".env.production", 0),
        row("sub:B/1", "B", "critical", ".env.production", 6),
        row("sub:B/1", "B", "low", ".env.example", 6),
        row("sub:C/1", "C", "critical", ".env.production", 6),
    ];
    assert_eq!(
        members(&fixes_of(&rows)),
        vec![names(&["sub:B/1"]), names(&["sub:C/1", "sub:FILE/1"])]
    );
    // A whole-file group less severe than the lines is no container: C stays alone.
    let rows = vec![
        row("sub:FILE/1", "FILE", "low", ".env.production", 0),
        row("sub:B/1", "B", "critical", ".env.production", 6),
        row("sub:B/1", "B", "low", ".env.example", 6),
        row("sub:C/1", "C", "critical", ".env.production", 6),
    ];
    assert_eq!(
        members(&fixes_of(&rows)),
        vec![names(&["sub:B/1", "sub:C/1"]), names(&["sub:FILE/1"])]
    );
}

#[test]
fn a_whole_file_fix_and_the_fix_holding_its_lines_cross_reference() {
    // The file group is less severe than the lines, so it cannot absorb them; it still lists the
    // place (a.ts, line 6) its whole file contains, and so does the other Fix.
    let rows = vec![
        row("sub:FILE/1", "FILE", "low", "a.ts", 0),
        row("sub:L/1", "L", "critical", "a.ts", 6),
        row("sub:L/1", "L", "critical", "b.ts", 1),
    ];
    let fixes = fixes_of(&rows);
    assert_eq!(fixes.len(), 2);
    // A Fix that also holds a.ts:6 itself shares that one place once, not once for the line and
    // again for its whole file.
    let mut rows = rows;
    rows.push(row("sub:FILE/1", "FILE", "low", "a.ts", 6));
    let both = fixes_of(&rows);
    assert_eq!(both.len(), 2);
    assert_eq!(both[0].shares.len(), 1);
    assert_eq!(both[0].shares[0].places, 1, "{:?}", both[0].shares);
    assert_eq!(both[1].shares[0].places, 1, "{:?}", both[1].shares);
    let rows = &rows[..3];
    let fixes = fixes_of(rows);
    let (file, lines) = (&fixes[0], &fixes[1]);
    assert_eq!(file.members, names(&["sub:FILE/1"]));
    assert_eq!(
        file.shares,
        vec![FixShare {
            fix: lines.id.clone(),
            places: 1
        }]
    );
    assert_eq!(
        lines.shares,
        vec![FixShare {
            fix: file.id.clone(),
            places: 1
        }]
    );
}
