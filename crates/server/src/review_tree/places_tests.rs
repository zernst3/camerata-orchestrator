//! The Place (D2, R-6a): one review row for each exact (repo, path, line), problem rows only, the
//! best-ranked row carrying the others. Synthetic fixtures only.

use camerata_api_types::audit::{
    ConventionObservedKind, FindingKind, NotAssessedKind, NotAssessedReason,
};

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

fn row(rule: &str, severity: &str, path: &str, line: usize) -> Finding {
    Finding {
        repo: "acme/app".to_string(),
        path: path.to_string(),
        line,
        rule_id: rule.to_string(),
        severity: severity.to_string(),
        status: "active".to_string(),
        snippet: format!("{rule} {path}:{line}"),
        site_key: format!("key:{rule}:{path}:{line}"),
        group_path: vec![
            "cat:x".to_string(),
            format!("rule:{rule}"),
            format!("sub:{rule}/1"),
        ],
        ..Finding::default()
    }
}

/// A tree whose sub-groups are the rows' sub-groups, with the Fixes worked out.
fn tree_of(rows: &[Finding]) -> ReviewTree {
    let mut ids: Vec<String> = rows
        .iter()
        .filter_map(|f| f.group_path.get(2).cloned())
        .collect();
    ids.sort();
    ids.dedup();
    let mut tree = ReviewTree {
        nodes: ids.iter().map(|i| node(i)).collect(),
        ..ReviewTree::default()
    };
    tree.refresh_fixes(rows);
    tree
}

fn places_of(rows: &[Finding]) -> Vec<PlaceNode> {
    merge_places(&tree_of(rows), rows)
}

#[test]
fn six_rules_of_four_areas_at_one_line_are_one_place_headed_by_the_best_ranked_row() {
    let mut rows = vec![
        row("SEC-A-1", "high", ".env.production", 6),
        row("SEC-B-1", "medium", ".env.production", 6),
        row("SEC-C-1", "critical", ".env.production", 6),
        row("SEC-D-1", "low", ".env.production", 6),
        row("SEC-E-1", "high", ".env.production", 6),
        row("SEC-F-1", "medium", ".env.production", 6),
    ];
    for (i, f) in rows.iter_mut().enumerate() {
        f.group_path[0] = format!("cat:area{}", i % 4);
    }
    let places = places_of(&rows);
    assert_eq!(places.len(), 1, "{places:?}");
    let place = &places[0];
    assert_eq!(place.carrier, "key:SEC-C-1:.env.production:6");
    assert_eq!(place.members[0], place.carrier);
    assert_eq!(place.members.len(), 6);
    assert_eq!(
        place.rules,
        ["SEC-A-1", "SEC-B-1", "SEC-C-1", "SEC-D-1", "SEC-E-1", "SEC-F-1"]
    );
    assert_eq!(place.node_id(), format!("place:{}", place.id));
    assert_eq!(place.id.len(), 12);
    // The best rank maximum, then the rest in rank order.
    let n = rows.len();
    let rank_of = |key: &str| {
        let i = rows.iter().position(|f| f.site_key == key).unwrap();
        crate::ai_audit::primary_rank(&rows[i], n, i)
    };
    let ranks: Vec<_> = place.members.iter().map(|k| rank_of(k)).collect();
    assert!(ranks.windows(2).all(|w| w[0] >= w[1]), "{ranks:?}");
}

#[test]
fn rows_at_different_lines_inside_the_window_with_a_shared_object_are_two_places() {
    let mut a = row("SEC-A-1", "high", "src/handler.ts", 10);
    let mut b = row("SEC-B-1", "high", "src/handler.ts", 13);
    a.captures
        .insert("table".to_string(), "profiles".to_string());
    b.captures
        .insert("table".to_string(), "profiles".to_string());
    let c = row("SEC-C-1", "high", "src/handler.ts", 13);
    // Two lines, one row at the first: no place holds two rows except line 13.
    let rows = vec![a, b, c];
    let places = places_of(&rows);
    assert_eq!(places.len(), 1, "{places:?}");
    assert_eq!(places[0].line, 13);
    assert!(
        !places[0]
            .members
            .contains(&"key:SEC-A-1:src/handler.ts:10".to_string()),
        "nothing merges across lines"
    );
    // With a single row at each line there is no place at all.
    let lone = vec![
        rows[0].clone(),
        row("SEC-B-1", "high", "src/handler.ts", 13),
    ];
    assert!(places_of(&lone).is_empty());
}

#[test]
fn a_whole_file_row_is_its_own_place_even_when_it_shares_a_fix_with_its_lines() {
    let rows = vec![
        row("SEC-FILE-1", "critical", ".env.production", 0),
        row("SEC-KEY-1", "high", ".env.production", 6),
        row("SEC-TOK-1", "high", ".env.production", 6),
    ];
    let tree = tree_of(&rows);
    // The Fix holds the file and its lines; the places do not.
    assert_eq!(tree.fixes.len(), 1, "{:?}", tree.fixes);
    let places = merge_places(&tree, &rows);
    assert_eq!(places.len(), 1, "{places:?}");
    assert_eq!(places[0].line, 6);
    assert!(
        !places[0]
            .members
            .iter()
            .any(|m| m == "key:SEC-FILE-1:.env.production:0"),
        "the whole-file row stays out of its lines' place"
    );
    assert_eq!(places[0].fix_ids, vec![tree.fixes[0].id.clone()]);
}

#[test]
fn conventions_not_assessed_false_positives_and_thrown_out_rows_never_carry_or_get_carried() {
    let mut convention = row("CONV-1", "low", "src/a.ts", 5);
    convention.kind = Some(FindingKind::ConventionObserved(ConventionObservedKind {
        convention_option_id: "o".to_string(),
        convention_label: "l".to_string(),
    }));
    let mut not_assessed = row("NA-1", "low", "src/a.ts", 5);
    not_assessed.kind = Some(FindingKind::NotAssessed(NotAssessedKind {
        reason: NotAssessedReason::PassFailed,
        reason_detail: String::new(),
    }));
    let mut thrown = row("THROWN-1", "high", "src/a.ts", 5);
    crate::engine_discard::mark(&mut thrown, crate::engine_discard::DISCARD_VERDICT);
    let mut thrown_by_path = row("THROWN-2", "high", "src/a.ts", 5);
    thrown_by_path.group_path = vec![
        "cat:x/thrown-out".to_string(),
        "rule:THROWN-2".to_string(),
        "sub:x/thrown-out/1".to_string(),
    ];
    let mut suppressed = row("SUP-1", "high", "src/a.ts", 5);
    suppressed.status = "suppressed-inline".to_string();
    let false_positive = row("FP-1", "high", "src/a.ts", 5);
    let real = row("REAL-1", "medium", "src/a.ts", 5);
    let rows = vec![
        convention,
        not_assessed,
        thrown,
        thrown_by_path,
        suppressed,
        false_positive,
        real,
    ];
    let tree = tree_of(&rows);
    // Only the false positive and the one real row are problems left; the false positive is left
    // out by the export that knows it.
    let none = merge_places_where(&tree, &rows, &|f| f.rule_id == "FP-1");
    assert!(none.is_empty(), "{none:?}");
    // Without that, the two problem rows are one place and nothing else is in it.
    let with_fp = merge_places(&tree, &rows);
    assert_eq!(with_fp.len(), 1, "{with_fp:?}");
    assert_eq!(with_fp[0].rules, ["FP-1", "REAL-1"]);
}

#[test]
fn a_single_row_place_is_not_stored_and_cannot_be_decided() {
    let rows = vec![
        row("SEC-A-1", "high", "src/a.ts", 3),
        row("SEC-B-1", "high", "src/b.ts", 4),
    ];
    let mut tree = tree_of(&rows);
    tree.refresh_places(&rows);
    assert!(tree.places.is_empty());
    let lone = place_id_of("acme/app", "src/a.ts", 3);
    assert!(!tree.is_decidable(&format!("{PLACE_PREFIX}{lone}")));

    let mut two = rows.clone();
    two.push(row("SEC-C-1", "low", "src/a.ts", 3));
    let mut tree = tree_of(&two);
    tree.refresh_places(&two);
    assert_eq!(tree.places.len(), 1);
    let id = tree.places[0].node_id();
    assert!(tree.is_decidable(&id));
    assert!(!tree.is_decidable("place:nothing"));
    let mut expected = tree.places[0].members.clone();
    expected.sort();
    assert_eq!(tree.batch_members(&id), expected);
}

#[test]
fn the_place_block_is_integral() {
    let rows = vec![
        row("SEC-A-1", "high", "src/a.ts", 3),
        row("SEC-B-1", "high", "src/a.ts", 3),
        row("SEC-C-1", "high", "src/a.ts", 3),
        row("SEC-D-1", "high", "src/b.ts", 4),
        row("SEC-E-1", "high", "src/c.ts", 0),
        row("SEC-F-1", "high", "src/c.ts", 0),
    ];
    let mut tree = tree_of(&rows);
    let block = tree.refresh_places(&rows);
    assert_eq!(block.stage, PLACE_MERGE_STAGE);
    assert_eq!(
        (block.rows_in, block.places_out, block.carried),
        (6, 3, 3),
        "{block:?}"
    );
    assert!(block.is_integral());
    assert_eq!(tree.place_merge_block(&rows), block);
}

#[test]
fn the_place_id_is_the_same_when_a_worse_row_appears_and_takes_the_carrier() {
    let first = vec![
        row("SEC-A-1", "medium", "src/a.ts", 3),
        row("SEC-B-1", "low", "src/a.ts", 3),
    ];
    let before = places_of(&first);
    let mut second = first.clone();
    second.push(row("SEC-C-1", "critical", "src/a.ts", 3));
    let after = places_of(&second);
    assert_eq!(before[0].id, after[0].id);
    assert_eq!(before[0].carrier, "key:SEC-A-1:src/a.ts:3");
    assert_eq!(after[0].carrier, "key:SEC-C-1:src/a.ts:3");
    assert_eq!(after[0].members.len(), 3);
}

#[test]
fn the_ledger_records_the_place_merge_when_the_tree_is_built() {
    let rows = vec![
        row("SEC-A-1", "high", "src/a.ts", 3),
        row("SEC-B-1", "medium", "src/a.ts", 3),
        row("SEC-C-1", "high", "src/b.ts", 9),
    ];
    let mut report = crate::onboard::build_report(vec!["acme/app".into()], Vec::new(), 1, rows);
    report.attach_tree();
    let block = report.ledger.place_merge().expect("recorded");
    assert_eq!(block.stage, PLACE_MERGE_STAGE);
    assert_eq!(
        (block.rows_in, block.places_out, block.carried),
        (3, 2, 1),
        "{block:?}"
    );
    assert!(block.is_integral());
    assert_eq!(report.tree.as_ref().unwrap().places.len(), 1);
}
