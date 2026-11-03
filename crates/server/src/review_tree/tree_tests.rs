//! The grouping, tree, collapse and link tests of the review tree (design 3.7 and 3A).

use std::collections::BTreeSet;

use super::*;
use crate::inspections::backfill_site_keys;
use crate::onboard::{Finding, MergedLocation};

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

/// A row from an external tool.
fn tool_row(rule_id: &str, tool: &str, path: &str, line: usize, snippet: &str) -> Finding {
    Finding {
        preview: true,
        preview_tool: Some(tool.to_string()),
        ..row(rule_id, path, line, snippet)
    }
}

/// A row from the model, with a calibration verdict.
fn ai_row(
    rule_id: &str,
    category: Option<&str>,
    path: &str,
    line: usize,
    snippet: &str,
) -> Finding {
    Finding {
        confidence: Some("high".to_string()),
        category: category.map(str::to_string),
        ..row(rule_id, path, line, snippet)
    }
}

/// Group `rows` the way the scan does and assemble the tree the way sealing does.
fn grouped(rows: Vec<Finding>) -> (Vec<Finding>, ReviewTree) {
    let tools = ToolMap::builtin();
    let mut alloc = SubgroupAllocator::new();
    let (mut rows, sub_nodes) = group_semantic(rows, &[], &tools, &mut alloc);
    let prev = ReviewTree {
        nodes: sub_nodes,
        ..ReviewTree::default()
    };
    backfill_site_keys(&mut rows);
    let tree = build_tree(&mut rows, &mut [], Some(prev), &tools, "run-1");
    (rows, tree)
}

/// The tree's membership against the rows: every node's members together are exactly the rows'
/// keys, no key is in two nodes and none is in zero, and every row sits in the sub-group whose
/// members name it.
pub(super) fn assert_tree_covers_rows(rows: &[Finding], tree: &ReviewTree) {
    let mut seen: std::collections::HashMap<&str, &str> = std::collections::HashMap::new();
    for node in &tree.nodes {
        for key in &node.members {
            let twice = seen.insert(key.as_str(), node.id.as_str());
            assert!(
                twice.is_none(),
                "{key:?} is in two nodes: {twice:?} and {}",
                node.id
            );
        }
    }
    let row_keys: BTreeSet<&str> = rows.iter().map(|f| f.site_key.as_str()).collect();
    assert_eq!(row_keys.len(), rows.len(), "row keys are unique");
    let member_keys: BTreeSet<&str> = seen.keys().copied().collect();
    assert_eq!(
        member_keys, row_keys,
        "the union of every node's members is the set of row keys"
    );
    for f in rows {
        assert_eq!(
            seen.get(f.site_key.as_str()).copied(),
            f.group_path.get(2).map(String::as_str),
            "{} is a member of the sub-group its group_path names",
            f.site_key
        );
    }
}

/// How many rows [`rows_with_known_duplicates`] collapses: the exact copy and the tool row (2),
/// the six-row same-rule group (5), and the nested pair (1).
const KNOWN_COLLAPSED: usize = 8;

/// Mixed rows plus KNOWN duplicates, so the collapse has exactly [`KNOWN_COLLAPSED`] sources to
/// fold:
/// - an exact copy of the first row, and the semgrep rule that enforces the same native rule on
///   the same line (2);
/// - an n-site group: six rows of one rule at one place (5);
/// - a nested pair: two rows of one rule at one place, the second already carrying `flagged_by`
///   sources of its own (1, and the carried sources must survive).
fn rows_with_known_duplicates() -> Vec<Finding> {
    let mut input = mixed_rows();
    let copy = input[0].clone();
    input.push(copy);
    input.push(tool_row(
        "camerata.security.sql-string-concat-python",
        "semgrep",
        "src/db.rs",
        input[0].line,
        &input[0].snippet.clone(),
    ));
    for i in 0..6 {
        input.push(row(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/n.rs",
            5,
            &format!("query(&format!(\"select {{id}} /* variant {i} */\"))"),
        ));
    }
    input.push(row(
        "SEC-NO-RAW-SQL-CONCAT-1",
        "src/nested.rs",
        7,
        "query(nested)",
    ));
    let mut carrying = row(
        "SEC-NO-RAW-SQL-CONCAT-1",
        "src/nested.rs",
        7,
        "query(nested)",
    );
    carrying.flagged_by = vec![
        FlaggedBy {
            rule_id: "tool-a".to_string(),
            tool: Some("semgrep".to_string()),
            site_key: "carried-key-a".to_string(),
            path: "src/nested.rs".to_string(),
            line: 8,
        },
        FlaggedBy {
            rule_id: "tool-b".to_string(),
            tool: Some("eslint".to_string()),
            site_key: "carried-key-b".to_string(),
            path: "src/nested.rs".to_string(),
            line: 9,
        },
    ];
    input.push(carrying);
    input
}

/// What came out of the scan's two stages over `input`.
struct Piped {
    input_keys: Vec<String>,
    carried_keys: Vec<String>,
    collapse: CollapseOutcome,
    rows: Vec<Finding>,
    tree: ReviewTree,
}

fn pipe(input: Vec<Finding>) -> Piped {
    let input_keys = crate::inspections::site_keys_of(&input);
    let carried_keys: Vec<String> = input
        .iter()
        .flat_map(|f| f.flagged_by.iter().map(|b| b.site_key.clone()))
        .collect();
    let tools = ToolMap::builtin();
    let mut collapse = collapse_identical(input, &tools);
    let (mut rows, sub_nodes) = group_semantic(
        std::mem::take(&mut collapse.rows),
        &[],
        &tools,
        &mut SubgroupAllocator::new(),
    );
    let prev = ReviewTree {
        nodes: sub_nodes,
        ..ReviewTree::default()
    };
    backfill_site_keys(&mut rows);
    let tree = build_tree(&mut rows, &mut [], Some(prev), &tools, "run-1");
    Piped {
        input_keys,
        carried_keys,
        collapse,
        rows,
        tree,
    }
}

/// Every input site is still there: as a row, or inside a `flagged_by` entry of the row it was
/// collapsed into.
fn assert_every_input_site_is_present(p: &Piped) {
    let row_keys: BTreeSet<&str> = p.rows.iter().map(|f| f.site_key.as_str()).collect();
    let flagged: BTreeSet<&str> = p
        .rows
        .iter()
        .flat_map(|f| f.flagged_by.iter().map(|b| b.site_key.as_str()))
        .collect();
    for key in p.input_keys.iter().chain(&p.carried_keys) {
        assert!(
            row_keys.contains(key.as_str()) || flagged.contains(key.as_str()),
            "input site {key:?} is neither a row nor a flagged_by entry"
        );
    }
}

/// Rows spread over files, rules and categories, with clusters, links and a lone AI row.
fn mixed_rows() -> Vec<Finding> {
    let mut rows = vec![
        row(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/db.rs",
            10,
            "query(&format!(\"select {id}\"))",
        ),
        row(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/db.rs",
            12,
            "query(&format!(\"delete {id}\"))",
        ),
        row(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/other.rs",
            3,
            "query(&format!(\"update {id}\"))",
        ),
        row(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/cfg.rs",
            4,
            "let key = \"sk_live_abcdef123456\";",
        ),
        row(
            "ARCH-MIDDLEWARE-FIRST-1",
            "src/app.rs",
            7,
            "app.use(cors())",
        ),
        ai_row(
            "AI-CORS-ANY-ORIGIN",
            Some("resource-exposure"),
            "src/app.rs",
            7,
            "app.use(cors())",
        ),
        ai_row(
            "AI-WEIRD-THING",
            None,
            "src/misc.rs",
            20,
            "do_the_weird_thing()",
        ),
        tool_row(
            "camerata.security.sql-string-concat-python",
            "semgrep",
            "svc/q.py",
            9,
            "cur.execute(\"select \" + x)",
        ),
    ];
    for r in &mut rows {
        r.detail = format!("{} detail", r.rule_id);
    }
    rows
}

#[test]
fn grouping_never_changes_the_row_count() {
    let input = rows_with_known_duplicates();
    let rows_in = input.len();
    let p = pipe(input);
    // The collapse folds exactly the known duplicates (exact copy, tool row, six-row n-site
    // group, nested pair) and nothing else; and it must fold something.
    assert!(
        !p.collapse.collapsed.is_empty(),
        "the fixture must collapse something"
    );
    assert_eq!(
        p.collapse.collapsed.len(),
        KNOWN_COLLAPSED,
        "{:?}",
        p.collapse.collapsed
    );
    assert_eq!(
        p.rows.len(),
        rows_in - p.collapse.collapsed.len(),
        "rows_out == rows_in - collapsed"
    );
    assert_eq!(p.rows.len(), rows_in - KNOWN_COLLAPSED);
    // Grouping then returned every row it was given, and the tree holds each exactly once.
    assert_tree_covers_rows(&p.rows, &p.tree);
    assert_eq!(
        p.tree.nodes.iter().map(|n| n.members.len()).sum::<usize>(),
        p.rows.len()
    );
    assert_every_input_site_is_present(&p);
    // The collapsed sources are all on the first row, which kept the floor text.
    assert_eq!(p.rows[0].rule_id, "SEC-NO-RAW-SQL-CONCAT-1");
    assert_eq!(p.rows[0].flagged_by.len(), 2);
    // The n-site group is one row holding five sources; the nested pair holds the two sources it
    // carried plus the row folded into it.
    let n_site = p.rows.iter().find(|f| f.path == "src/n.rs").unwrap();
    assert_eq!(n_site.flagged_by.len(), 5);
    let nested = p.rows.iter().find(|f| f.path == "src/nested.rs").unwrap();
    assert_eq!(nested.flagged_by.len(), 3);
    // Grouping alone (no collapse) keeps the same rows in the same order at the same severity.
    let input = mixed_rows();
    let (rows, _tree) = grouped(input.clone());
    assert_eq!(rows.len(), input.len(), "every row in is a row out");
    for (before, after) in input.iter().zip(&rows) {
        assert_eq!(before.rule_id, after.rule_id);
        assert_eq!(before.path, after.path);
        assert_eq!(before.line, after.line);
        assert_eq!(before.severity, after.severity);
    }
}

#[test]
fn every_row_has_a_three_level_group_path() {
    let (rows, tree) = grouped(mixed_rows());
    for f in &rows {
        assert_eq!(f.group_path.len(), 3, "{}: {:?}", f.rule_id, f.group_path);
        assert!(f.group_path[0].starts_with("cat:"), "{:?}", f.group_path);
        assert!(f.group_path[1].starts_with("rule:"), "{:?}", f.group_path);
        assert!(f.group_path[2].starts_with("sub:"), "{:?}", f.group_path);
        // The path names real nodes, each the parent of the next.
        let cat = tree.node(&f.group_path[0]).expect("category node");
        let rule = tree.node(&f.group_path[1]).expect("rule node");
        let sub = tree.node(&f.group_path[2]).expect("sub-group node");
        assert_eq!(rule.parent.as_deref(), Some(cat.id.as_str()));
        assert_eq!(sub.parent.as_deref(), Some(rule.id.as_str()));
        assert_eq!(cat.level, NodeLevel::Category);
        assert_eq!(rule.level, NodeLevel::Rule);
        assert_eq!(sub.level, NodeLevel::Subgroup);
    }
}

#[test]
fn every_tree_member_key_exists_exactly_once_in_findings() {
    let (rows, tree) = grouped(mixed_rows());
    let member_keys: Vec<&String> = tree.nodes.iter().flat_map(|n| n.members.iter()).collect();
    let finding_keys: Vec<&String> = rows.iter().map(|f| &f.site_key).collect();
    assert_eq!(member_keys.len(), finding_keys.len(), "one member per row");
    let members: BTreeSet<&String> = member_keys.iter().copied().collect();
    let keys: BTreeSet<&String> = finding_keys.iter().copied().collect();
    assert_eq!(members.len(), member_keys.len(), "no key is a member twice");
    assert_eq!(members, keys, "every row is a member, and only rows are");
    // Only a sub-group holds members directly.
    assert!(tree
        .nodes
        .iter()
        .filter(|n| n.level != NodeLevel::Subgroup)
        .all(|n| n.members.is_empty()));
}

#[test]
fn external_tool_row_groups_under_the_native_rule_and_keeps_its_own_rule_id() {
    let (rows, tree) = grouped(vec![
        row(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/db.rs",
            10,
            "query(&format!(\"select {id}\"))",
        ),
        tool_row(
            "camerata.security.sql-string-concat-python",
            "semgrep",
            "svc/q.py",
            9,
            "cur.execute(\"select \" + x)",
        ),
    ]);
    let tool = rows.iter().find(|f| f.preview_tool.is_some()).unwrap();
    assert_eq!(
        tool.rule_id, "camerata.security.sql-string-concat-python",
        "the row keeps the tool's own rule id"
    );
    assert_eq!(tool.group_path[1], "rule:SEC-NO-RAW-SQL-CONCAT-1");
    let native = rows.iter().find(|f| f.preview_tool.is_none()).unwrap();
    assert_eq!(
        native.group_path[1], tool.group_path[1],
        "one rule node for both"
    );
    let rule = tree.node("rule:SEC-NO-RAW-SQL-CONCAT-1").unwrap();
    assert!(rule
        .rule_ids
        .contains(&"camerata.security.sql-string-concat-python".to_string()));
    assert!(rule
        .rule_ids
        .contains(&"SEC-NO-RAW-SQL-CONCAT-1".to_string()));

    // A tool id with no native counterpart gets its own rule node, never the native one.
    let (rows, _) = grouped(vec![tool_row(
        "no-such-lint",
        "eslint",
        "a.js",
        1,
        "var a = 1;",
    )]);
    assert_eq!(rows[0].group_path[1], "rule:eslint/no-such-lint");
    assert_eq!(rows[0].group_path[0], "cat:other-tools");
}

#[test]
fn ai_named_row_groups_under_its_category_node() {
    let (rows, tree) = grouped(vec![ai_row(
        "AI-ADMIN-ROUTE-NO-CHECK",
        Some("authorization"),
        "src/admin.rs",
        30,
        "router.get(\"/admin\", list_users)",
    )]);
    assert_eq!(rows[0].group_path[0], "cat:authorization");
    assert_eq!(rows[0].group_path[1], "rule:ai/authorization");
    let rule = tree.node("rule:ai/authorization").unwrap();
    assert_eq!(rule.label, "Model-named: authorization");
    assert!(rule
        .rule_ids
        .contains(&"AI-ADMIN-ROUTE-NO-CHECK".to_string()));
}

#[test]
fn unmapped_row_lands_in_uncategorised_never_dropped() {
    let (rows, tree) = grouped(vec![
        // A model row with no category.
        ai_row("AI-WHO-KNOWS", None, "src/x.rs", 5, "mystery()"),
        // A model row with a category outside the closed set.
        ai_row(
            "AI-OTHER",
            Some("made-up-category"),
            "src/y.rs",
            6,
            "other()",
        ),
        // A native id nothing can classify.
        row("XYZ-1", "src/z.rs", 7, "zed()"),
    ]);
    assert_eq!(rows.len(), 3, "nothing is dropped");
    for f in &rows {
        assert_eq!(f.group_path[0], "cat:uncategorised", "{}", f.rule_id);
    }
    assert!(tree.node("cat:uncategorised").is_some());
}

#[test]
fn also_locations_unfold_to_rows_with_their_own_site_keys_on_load() {
    let mut survivor = row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 10, "query(a)");
    survivor.detail = "Raw SQL, the explanation".to_string();
    survivor.fix_specific = Some("Bind the parameter.".to_string());
    survivor.also_locations = vec![
        MergedLocation::site(
            "acme/app",
            "src/b.rs",
            20,
            "SEC-NO-RAW-SQL-CONCAT-1",
            "query(b)",
            false,
        )
        .with_rating("high", "Raw SQL in b"),
        MergedLocation::site("acme/app", "src/c.rs", 30, "AI-RAW-SQL", "query(c)", true),
    ];
    let mut report =
        crate::onboard::build_report(vec!["acme/app".to_string()], Vec::new(), 3, vec![survivor]);
    report.normalize_loaded();

    assert_eq!(
        report.findings.len(),
        3,
        "the survivor and its two absorbed sites"
    );
    // The unfold is its own ledger stage, so the row arithmetic closes exactly:
    // rows exported + collapsed == detected + unfolded.
    let unfold: Vec<_> = report
        .ledger
        .stages()
        .iter()
        .filter(|s| s.stage == UNFOLD_STAGE)
        .collect();
    assert_eq!(unfold.len(), 1, "{:#?}", report.ledger.stages());
    assert_eq!((unfold[0].rows_in, unfold[0].rows_out), (1, 3));
    assert_eq!(unfold[0].unaccounted, 0);
    assert_eq!(
        report.findings.len(),
        unfold[0].rows_in + (unfold[0].rows_out - unfold[0].rows_in),
    );
    assert!(report.findings.iter().all(|f| f.also_locations.is_empty()));
    let keys: BTreeSet<&String> = report.findings.iter().map(|f| &f.site_key).collect();
    assert_eq!(keys.len(), 3, "each row has its own site key");
    assert!(report.findings.iter().all(|f| !f.site_key.is_empty()));
    let b = report
        .findings
        .iter()
        .find(|f| f.path == "src/b.rs")
        .unwrap();
    assert_eq!(
        (b.line, b.severity.as_str(), b.snippet.as_str()),
        (20, "high", "query(b)")
    );
    assert!(b.detail.contains("Raw SQL in b") && b.detail.contains("the explanation"));
    assert_eq!(b.fix_specific.as_deref(), Some("Bind the parameter."));
    let c = report
        .findings
        .iter()
        .find(|f| f.path == "src/c.rs")
        .unwrap();
    assert_eq!(c.rule_id, "AI-RAW-SQL");
    assert_eq!(
        c.severity, "medium",
        "a site with no recorded severity takes the survivor's"
    );
    // Every promoted row is in the tree, in the survivor's rule node.
    let tree = report.tree.as_ref().unwrap();
    let members: usize = tree.nodes.iter().map(|n| n.members.len()).sum();
    assert_eq!(members, 3);
    // And loading it again changes nothing.
    let before = report.findings.clone();
    report.normalize_loaded();
    assert_eq!(report.findings, before);
}

#[test]
fn rebuilding_the_tree_from_the_same_rows_gives_the_same_tree() {
    let (mut rows, tree) = grouped(mixed_rows());
    let again = build_tree(
        &mut rows,
        &mut [],
        Some(tree.clone()),
        &ToolMap::builtin(),
        "run-1",
    );
    assert_eq!(again, tree);
    // And a report that never had a tree gets the same one from its rows alone.
    let fresh = build_tree(&mut rows, &mut [], None, &ToolMap::builtin(), "run-1");
    assert_eq!(
        fresh.nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>(),
        tree.nodes.iter().map(|n| n.id.clone()).collect::<Vec<_>>()
    );
}

// ---- 3A: collapse and links ---------------------------------------------------------------

fn collapse(rows: Vec<Finding>) -> CollapseOutcome {
    collapse_identical(rows, &ToolMap::builtin())
}

#[test]
fn collapse_requires_same_family_same_file_and_same_place() {
    let base = row(
        "SEC-NO-RAW-SQL-CONCAT-1",
        "src/a.rs",
        10,
        "query(&format!(\"select {id}\"))",
    );

    // Same family, same file, same line: one row.
    let out = collapse(vec![
        base.clone(),
        row(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/a.rs",
            10,
            "query(&format!(\"select {id}\"))",
        ),
    ]);
    assert_eq!(out.rows.len(), 1);

    // Within the window, one snippet contains the other: one row.
    let out = collapse(vec![
        base.clone(),
        row(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/a.rs",
            13,
            "query(&format!(\"select {id}\")).await",
        ),
    ]);
    assert_eq!(
        out.rows.len(),
        1,
        "a nearby line quoting the same code is the same site"
    );

    // Within the window but the code is different: two rows.
    let out = collapse(vec![
        base.clone(),
        row(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/a.rs",
            13,
            "completely_different_statement()",
        ),
    ]);
    assert_eq!(out.rows.len(), 2, "different code is a different site");

    // Beyond the window: two rows.
    let out = collapse(vec![
        base.clone(),
        row(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/a.rs",
            40,
            "query(&format!(\"select {id}\"))",
        ),
    ]);
    assert_eq!(out.rows.len(), 2);

    // Another file: two rows.
    let out = collapse(vec![
        base.clone(),
        row(
            "SEC-NO-RAW-SQL-CONCAT-1",
            "src/b.rs",
            10,
            "query(&format!(\"select {id}\"))",
        ),
    ]);
    assert_eq!(out.rows.len(), 2);

    // Another repo: two rows.
    let mut other_repo = row(
        "SEC-NO-RAW-SQL-CONCAT-1",
        "src/a.rs",
        10,
        "query(&format!(\"select {id}\"))",
    );
    other_repo.repo = "acme/other".to_string();
    let out = collapse(vec![base.clone(), other_repo]);
    assert_eq!(out.rows.len(), 2);

    // Another rule family at the same place: two rows (they are linked, not collapsed).
    let out = collapse(vec![
        base.clone(),
        row(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            10,
            "query(&format!(\"select {id}\"))",
        ),
    ]);
    assert_eq!(out.rows.len(), 2);

    // The external tool that enforces the native rule IS the same family.
    let out = collapse(vec![
        base.clone(),
        tool_row(
            "camerata.security.sql-string-concat-python",
            "semgrep",
            "src/a.rs",
            10,
            "query(&format!(\"select {id}\"))",
        ),
    ]);
    assert_eq!(out.rows.len(), 1);
    assert_eq!(
        out.rows[0].rule_id, "SEC-NO-RAW-SQL-CONCAT-1",
        "the floor row's text survives"
    );
    assert_eq!(out.rows[0].flagged_by[0].tool.as_deref(), Some("semgrep"));
}

#[test]
fn collapse_keeps_every_source_key_in_flagged_by() {
    let mut floor = row(
        "SEC-NO-RAW-SQL-CONCAT-1",
        "src/a.rs",
        10,
        "query(&format!(\"select {id}\"))",
    );
    floor.severity = "medium".to_string();
    let mut tool = tool_row(
        "camerata.security.sql-string-concat-python",
        "semgrep",
        "src/a.rs",
        10,
        "query(&format!(\"select {id}\"))",
    );
    tool.severity = "critical".to_string();
    let mut model = ai_row(
        "SEC-NO-RAW-SQL-CONCAT-1",
        Some("injection"),
        "src/a.rs",
        12,
        "query(&format!(\"select {id}\")).await",
    );
    model.severity = "high".to_string();
    let input = vec![floor, tool, model];
    let mut keyed = input.clone();
    backfill_site_keys(&mut keyed);
    let source_keys: BTreeSet<String> = keyed.iter().map(|f| f.site_key.clone()).collect();

    let out = collapse(input);
    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.collapsed.len(), 2);
    assert_eq!(
        out.rows.len() + out.collapsed.len(),
        3,
        "rows_in == rows_out + collapsed"
    );
    let survivor = &out.rows[0];
    assert_eq!(
        survivor.severity, "critical",
        "the maximum severity survives"
    );
    assert_eq!(
        survivor.flagged_by.len(),
        2,
        "one entry per collapsed source"
    );
    // Amendment A1: each source keeps its OWN path and line.
    let lines: BTreeSet<usize> = survivor.flagged_by.iter().map(|b| b.line).collect();
    assert_eq!(lines, BTreeSet::from([10, 12]));
    assert!(survivor.flagged_by.iter().all(|b| b.path == "src/a.rs"));
    // Every source key is the key that source had as a row.
    let flagged_keys: BTreeSet<String> = survivor
        .flagged_by
        .iter()
        .map(|b| b.site_key.clone())
        .collect();
    assert!(flagged_keys.is_subset(&source_keys));
    assert_eq!(flagged_keys.len(), 2);
    // The ledger accounting names every collapsed row.
    assert_eq!(out.accounting().accounted(), 2);
}

#[test]
fn ai_row_is_never_collapsed_into_a_native_family() {
    let native = row(
        "SEC-NO-RAW-SQL-CONCAT-1",
        "src/a.rs",
        10,
        "query(&format!(\"select {id}\"))",
    );
    let model = ai_row(
        "AI-RAW-SQL-HERE",
        Some("injection"),
        "src/a.rs",
        10,
        "query(&format!(\"select {id}\"))",
    );
    let out = collapse(vec![native.clone(), model.clone()]);
    assert_eq!(out.rows.len(), 2);
    assert!(out.collapsed.is_empty());
    // Even an AI row whose id merely looks like the native one.
    let lookalike = ai_row(
        "AI-SEC-NO-RAW-SQL-CONCAT-1",
        Some("injection"),
        "src/a.rs",
        10,
        "query(&format!(\"select {id}\"))",
    );
    let out = collapse(vec![native, lookalike]);
    assert_eq!(out.rows.len(), 2);
    // Two identical AI ids at one place are one row.
    let out = collapse(vec![model.clone(), model]);
    assert_eq!(out.rows.len(), 1);
}

#[test]
fn collapse_never_fuses_a_held_row_with_an_asserted_one_or_two_credentials() {
    let asserted = row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 10, "query(a)");
    let mut held = row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 10, "query(a)");
    held.needs_review = true;
    held.confidence = Some("needs-review".to_string());
    assert_eq!(collapse(vec![asserted, held]).rows.len(), 2);

    let one = row(
        "SEC-NO-HARDCODED-SECRETS-1",
        "src/c.rs",
        4,
        "let a = \"sk_live_AAAAAAAAAAAAAAAA1111\";",
    );
    let two = row(
        "SEC-NO-HARDCODED-SECRETS-1",
        "src/c.rs",
        6,
        "let b = \"sk_live_BBBBBBBBBBBBBBBB2222\";",
    );
    assert_eq!(
        collapse(vec![one, two]).rows.len(),
        2,
        "two credentials stay two rows"
    );
}

#[test]
fn link_never_changes_row_count_or_severity() {
    // Two different rule families on one line, and an AI row beside a native row.
    let input = vec![
        {
            let mut r = row(
                "ARCH-MIDDLEWARE-FIRST-1",
                "src/app.rs",
                7,
                "app.use(cors())",
            );
            r.severity = "low".to_string();
            r
        },
        {
            let mut r = ai_row(
                "AI-CORS-ANY-ORIGIN",
                Some("resource-exposure"),
                "src/app.rs",
                7,
                "app.use(cors())",
            );
            r.severity = "critical".to_string();
            r
        },
    ];
    let (rows, _) = grouped(input.clone());
    assert_eq!(rows.len(), input.len());
    for (before, after) in input.iter().zip(&rows) {
        assert_eq!(
            before.severity, after.severity,
            "a link never moves a severity"
        );
    }
    let linked: Vec<&Finding> = rows
        .iter()
        .filter(|f| f.possible_duplicate_of.is_some())
        .collect();
    assert_eq!(linked.len(), 1, "exactly one of the pair carries the link");
    let link = linked[0].possible_duplicate_of.as_ref().unwrap();
    assert_eq!(link.hint, "same_line");
    let target = rows.iter().find(|f| f.site_key == link.key).unwrap();
    assert_ne!(target.site_key, linked[0].site_key);
}

#[test]
fn batch_decision_does_not_follow_a_link() {
    let (rows, tree) = grouped(vec![
        row(
            "ARCH-MIDDLEWARE-FIRST-1",
            "src/app.rs",
            7,
            "app.use(cors())",
        ),
        ai_row(
            "AI-CORS-ANY-ORIGIN",
            Some("resource-exposure"),
            "src/app.rs",
            7,
            "app.use(cors())",
        ),
    ]);
    let linked = rows
        .iter()
        .find(|f| f.possible_duplicate_of.is_some())
        .unwrap();
    let target_key = linked.possible_duplicate_of.as_ref().unwrap().key.clone();
    let target = rows.iter().find(|f| f.site_key == target_key).unwrap();
    assert_ne!(
        linked.group_path[1], target.group_path[1],
        "the two sit in different rule nodes"
    );
    // A batch decision on the linked row's rule node covers that node's rows and no others.
    let covered = tree.batch_members(&linked.group_path[1]);
    assert_eq!(covered, vec![linked.site_key.clone()]);
    assert!(!covered.contains(&target_key));
}

#[test]
fn nodes_with_security_or_severe_rows_are_escalation_nodes() {
    let mut low = row("ARCH-STYLE-PREFERENCE-1", "a.rs", 1, "let x = 1;");
    low.severity = "low".to_string();
    let mut high = row("SEC-NO-RAW-SQL-CONCAT-1", "b.rs", 1, "query(x)");
    high.severity = "high".to_string();
    let (_, tree) = grouped(vec![low, high]);
    assert_eq!(
        tree.node("rule:ARCH-STYLE-PREFERENCE-1").unwrap().kind,
        NodeKind::Bulk
    );
    assert_eq!(
        tree.node("rule:SEC-NO-RAW-SQL-CONCAT-1").unwrap().kind,
        NodeKind::Escalation
    );
}

#[test]
fn collapse_never_launders_an_evidence_unverified_source() {
    use crate::onboard::evidence_check::{
        EVIDENCE_UNVERIFIED_CAPTURE, EVIDENCE_UNVERIFIED_SITES_CAPTURE,
    };
    let clean = row(
        "SEC-NO-RAW-SQL-CONCAT-1",
        "src/a.rs",
        10,
        "query(&format!(\"select {id}\"))",
    );
    let mut marked = tool_row(
        "camerata.security.sql-string-concat-python",
        "semgrep",
        "src/a.rs",
        12,
        "query(&format!(\"select {id}\"))",
    );
    marked.captures.insert(
        EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "quoted snippet not found".to_string(),
    );
    let out = collapse(vec![clean, marked]);
    assert_eq!(out.rows.len(), 1);
    let survivor = &out.rows[0];
    assert!(
        survivor.captures.contains_key(EVIDENCE_UNVERIFIED_CAPTURE),
        "the survivor stays held: {:?}",
        survivor.captures
    );
    assert_eq!(
        survivor
            .captures
            .get(EVIDENCE_UNVERIFIED_SITES_CAPTURE)
            .map(String::as_str),
        Some("src/a.rs:12")
    );
}

#[test]
fn a_held_or_evidence_unverified_row_stays_held_in_whatever_node_it_lands() {
    use crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE;
    let mut held = row("ARCH-STYLE-PREFERENCE-1", "a.rs", 3, "let x = 1;");
    held.needs_review = true;
    held.confidence = Some("needs-review".to_string());
    let mut unverified = row("SEC-NO-RAW-SQL-CONCAT-1", "b.rs", 4, "query(x)");
    unverified.captures.insert(
        EVIDENCE_UNVERIFIED_CAPTURE.to_string(),
        "not found".to_string(),
    );
    let (rows, _) = grouped(vec![held, unverified]);
    let held = rows.iter().find(|f| f.path == "a.rs").unwrap();
    assert!(held.needs_review && held.confidence.as_deref() == Some("needs-review"));
    let unverified = rows.iter().find(|f| f.path == "b.rs").unwrap();
    assert!(unverified
        .captures
        .contains_key(EVIDENCE_UNVERIFIED_CAPTURE));
}

#[test]
fn a_preview_of_another_family_on_the_same_line_is_not_folded_into_a_floor_row() {
    // The floor secret rule and the vendor-token rule share a security category but are two
    // rules: two rows. A tool rule that enforces the floor rule IS folded in, keeping its own
    // line in flagged_by.
    let mut existing = vec![row(
        "SEC-NO-HARDCODED-SECRETS-1",
        "src/c.rs",
        4,
        "let key = \"abc\";",
    )];
    let other_family = tool_row(
        "SEC-NO-VENDOR-TOKEN-1",
        "semgrep",
        "src/c.rs",
        4,
        "let key = \"abc\";",
    );
    let kept = crate::dedup_scan_previews(&mut existing, vec![other_family]);
    assert_eq!(kept.len(), 1, "another family is its own row");
    assert!(existing[0].flagged_by.is_empty());

    let enforcing = tool_row(
        "camerata.security.hardcoded-secret",
        "semgrep",
        "src/c.rs",
        4,
        "let key = \"abc\";",
    );
    let kept = crate::dedup_scan_previews(&mut existing, vec![enforcing]);
    assert!(kept.is_empty(), "the enforcing tool rule is folded in");
    assert_eq!(existing[0].flagged_by.len(), 1);
    assert_eq!(existing[0].flagged_by[0].tool.as_deref(), Some("semgrep"));
    assert_eq!(existing[0].flagged_by[0].line, 4);
}

#[test]
fn collapse_and_group_stages_reconcile_in_the_scan_ledger() {
    // The same two calls the scan makes, accounted the way the scan accounts them.
    let input = rows_with_known_duplicates();
    let rows_in = input.len();
    let p = pipe(input);
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    let rows_out = p.rows.len();
    let entry = ledger
        .record_stage_for_repo(
            "acme/app",
            "collapse-identical",
            rows_in,
            rows_out,
            p.collapse.accounting(),
        )
        .clone();
    assert_eq!(entry.unaccounted, 0, "{entry:?}");
    assert!(!p.collapse.collapsed.is_empty(), "something must collapse");
    assert_eq!(
        (entry.rows_in, entry.rows_out),
        (rows_in, rows_in - KNOWN_COLLAPSED)
    );
    assert_eq!(
        entry.merged_into.len(),
        KNOWN_COLLAPSED,
        "every collapsed row is named"
    );
    assert_eq!(
        rows_in,
        entry.rows_out + entry.merged_into.len(),
        "rows_in == rows_out + collapsed"
    );
    let entry = ledger
        .record_stage_for_repo(
            "acme/app",
            "group",
            rows_out,
            p.tree.nodes.iter().map(|n| n.members.len()).sum(),
            crate::scan_ledger::StageAccounting::default(),
        )
        .clone();
    assert_eq!(entry.unaccounted, 0, "every row is in a node: {entry:?}");
    assert_tree_covers_rows(&p.rows, &p.tree);
    assert_every_input_site_is_present(&p);
}

// ---- successors for the deleted absorbing-merge tests ----------------------------------------

#[test]
fn an_n_site_same_rule_collapse_reconciles_with_every_source_kept() {
    // Six rows of one rule at one place (n = 6): one survivor, five collapsed, five distinct
    // source keys, and the ledger accounts for every one.
    let input: Vec<Finding> = (0..6)
        .map(|i| {
            row(
                "SEC-NO-RAW-SQL-CONCAT-1",
                "src/a.rs",
                10,
                &format!("query(&format!(\"select {{id}} /* variant {i} */\"))"),
            )
        })
        .collect();
    let input_keys: BTreeSet<String> = crate::inspections::site_keys_of(&input)
        .into_iter()
        .collect();
    assert_eq!(input_keys.len(), 6);
    let out = collapse(input);
    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.collapsed.len(), 5);
    assert_eq!(
        out.rows.len() + out.collapsed.len(),
        6,
        "rows_in == rows_out + collapsed"
    );
    let survivor = &out.rows[0];
    assert_eq!(
        survivor.flagged_by.len(),
        5,
        "one entry per collapsed source"
    );
    let mut all: BTreeSet<String> = survivor
        .flagged_by
        .iter()
        .map(|b| b.site_key.clone())
        .collect();
    assert_eq!(all.len(), 5, "five distinct source keys");
    all.insert(crate::inspections::site_keys_of(&out.rows)[0].clone());
    // The survivor is the first row (earliest wins a tie), so every input key is accounted for.
    assert_eq!(
        all, input_keys,
        "every input site is the survivor or a flagged_by source"
    );
    let mut ledger = crate::scan_ledger::ScanLedger::new();
    let entry = ledger.record_stage("collapse-identical", 6, 1, out.accounting());
    assert_eq!(entry.unaccounted, 0, "{entry:?}");
    assert_eq!(entry.merged_into.len(), 5);
}

#[test]
fn a_collapse_over_rows_that_already_carry_flagged_by_loses_no_source() {
    let source = |rule: &str, line: usize| FlaggedBy {
        rule_id: rule.to_string(),
        tool: Some("semgrep".to_string()),
        site_key: format!("key-{rule}-{line}"),
        path: "src/a.rs".to_string(),
        line,
    };
    let mut a = row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 10, "query(x)");
    a.flagged_by = vec![source("tool-one", 11)];
    let mut b = row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 10, "query(x)");
    b.flagged_by = vec![source("tool-two", 12), source("tool-three", 13)];
    let c = row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 10, "query(x)");
    let out = collapse(vec![a, b, c]);
    assert_eq!(out.rows.len(), 1);
    let kept: BTreeSet<(String, usize)> = out.rows[0]
        .flagged_by
        .iter()
        .map(|b| (b.rule_id.clone(), b.line))
        .collect();
    // The survivor's own, the sources it had collected, and the two rows folded in.
    for expected in [("tool-one", 11), ("tool-two", 12), ("tool-three", 13)] {
        assert!(
            kept.contains(&(expected.0.to_string(), expected.1)),
            "{expected:?} was lost: {kept:?}"
        );
    }
    assert_eq!(
        out.rows[0].flagged_by.len(),
        3 + 2,
        "three carried sources plus the two rows collapsed in: {:?}",
        out.rows[0].flagged_by
    );
}

#[test]
fn unfold_never_lists_a_site_twice_and_never_loses_one() {
    let nested = || MergedLocation::site("acme/app", "src/other.rs", 9, "AI-NESTED", "x", false);
    let mut a = row("AI-IDOR-INVOICES", "src/h.rs", 3, "db.get(invoice_id)");
    a.also_locations = vec![nested(), nested()];
    let mut b = row("AI-OTHER-NAME", "src/h.rs", 3, "db.get(invoice_id)");
    b.also_locations = vec![nested()];
    // A site that is already a row of the same rule at the same place, rated higher.
    let mut c = row("AI-THIRD", "src/h.rs", 8, "db.get(other)");
    c.also_locations = vec![MergedLocation::site(
        "acme/app",
        "src/h.rs",
        3,
        "AI-IDOR-INVOICES",
        "db.get(invoice_id)",
        false,
    )
    .with_rating("critical", "Any user can read any invoice")];
    // The engine's own site identity: site key (rule plus snippet fingerprint) and line.
    let strong = |repo: &str, path: &str, rule: &str, snippet: &str, line: usize| {
        (
            camerata_api_types::site_key::site_key(repo, path, rule, snippet, 0),
            line,
        )
    };
    let before: BTreeSet<(String, usize)> = [&a, &b, &c]
        .iter()
        .flat_map(|f| {
            std::iter::once(strong(&f.repo, &f.path, &f.rule_id, &f.snippet, f.line)).chain(
                f.also_locations
                    .iter()
                    .map(|l| strong(&l.repo, &l.path, &l.rule_id, &l.snippet, l.line)),
            )
        })
        .collect();
    let mut rows = vec![a, b, c];
    let added = unfold_also_locations(&mut rows);
    assert_eq!(
        added, 1,
        "only the shared nested site is new, and it is promoted once"
    );
    assert_eq!(rows.len(), 4);
    assert!(rows.iter().all(|f| f.also_locations.is_empty()));
    let places: Vec<(String, usize)> = rows
        .iter()
        .map(|f| strong(&f.repo, &f.path, &f.rule_id, &f.snippet, f.line))
        .collect();
    let unique: BTreeSet<_> = places.iter().cloned().collect();
    assert_eq!(
        unique.len(),
        places.len(),
        "no site appears twice: {places:?}"
    );
    assert_eq!(unique, before, "no site is lost");
    // The duplicate that was rated higher raised the row that stands for the place.
    let idor = rows
        .iter()
        .find(|f| f.rule_id == "AI-IDOR-INVOICES")
        .unwrap();
    assert_eq!(idor.severity, "critical");
    // Loading again changes nothing.
    let again = rows.clone();
    assert_eq!(unfold_also_locations(&mut rows), 0);
    assert_eq!(rows, again);
}

#[test]
fn unfold_keeps_two_unlocated_sites_of_one_rule_that_differ_only_by_snippet() {
    // Two sites at line 0 of one file under one rule: only the snippet tells them apart (the
    // engine's own site identity). Both are promoted and both snippets and headlines survive.
    let mut survivor = row(
        "AI-NO-RATE-LIMIT",
        "src/api.rs",
        0,
        "no rate limit on login",
    );
    survivor.also_locations = vec![
        MergedLocation::site(
            "acme/app",
            "src/api.rs",
            0,
            "AI-NO-RATE-LIMIT",
            "no rate limit on export",
            true,
        )
        .with_rating("medium", "Export has no rate limit"),
        MergedLocation::site(
            "acme/app",
            "src/api.rs",
            0,
            "AI-NO-RATE-LIMIT",
            "no rate limit on search",
            true,
        )
        .with_rating("low", "Search has no rate limit"),
    ];
    let mut rows = vec![survivor];
    let added = unfold_also_locations(&mut rows);
    assert_eq!(added, 2, "{rows:#?}");
    let snippets: BTreeSet<&str> = rows.iter().map(|f| f.snippet.as_str()).collect();
    assert!(
        snippets.contains("no rate limit on export")
            && snippets.contains("no rate limit on search")
    );
    let details: String = rows
        .iter()
        .map(|f| f.detail.clone())
        .collect::<Vec<_>>()
        .join("|");
    assert!(
        details.contains("Export has no rate limit")
            && details.contains("Search has no rate limit")
    );
    // A duplicate that is a located consequence raises `located` on the row that stands for it.
    let mut unlocated = row("AI-X", "src/a.rs", 5, "code here");
    unlocated.located = false;
    let mut holder = row("AI-Y", "src/b.rs", 1, "other code");
    holder.also_locations = vec![MergedLocation::site(
        "acme/app",
        "src/a.rs",
        5,
        "AI-X",
        "code here",
        false,
    )];
    let mut rows = vec![unlocated, holder];
    unfold_also_locations(&mut rows);
    assert_eq!(rows.len(), 2);
    assert!(
        rows[0].located,
        "a located duplicate marks the standing row located"
    );
}

#[test]
fn the_same_structural_rule_in_two_repos_stays_separate_per_repo() {
    let make = |repo: &str, line: usize| {
        let mut f = row(
            "ARCH-SOME-PREFERENCE-1",
            "src/index.ts",
            line,
            "let x = shared_shape();",
        );
        f.repo = repo.to_string();
        f.category = Some("arch-conformance".to_string());
        f.confidence = Some("needs-review".to_string());
        f.needs_review = true;
        f.severity = "low".to_string();
        f
    };
    let p = pipe(vec![
        make("acme/web", 12),
        make("acme/web", 13),
        make("acme/api", 12),
        make("acme/api", 13),
    ]);
    assert_eq!(p.rows.len() + p.collapse.collapsed.len(), 4);
    assert_tree_covers_rows(&p.rows, &p.tree);
    let rule_nodes: BTreeSet<&str> = p.rows.iter().map(|f| f.group_path[1].as_str()).collect();
    assert_eq!(
        rule_nodes,
        BTreeSet::from(["rule:ARCH-SOME-PREFERENCE-1"]),
        "one shared rule node"
    );
    for f in &p.rows {
        for b in &f.flagged_by {
            assert!(
                b.site_key.starts_with(&format!("{}\u{0}", f.repo)),
                "collapsed across repos: {f:?}"
            );
        }
        if let Some(link) = &f.possible_duplicate_of {
            let target = p.rows.iter().find(|t| t.site_key == link.key).unwrap();
            assert_eq!(target.repo, f.repo, "linked across repos");
        }
    }
    let repos: BTreeSet<&str> = p.rows.iter().map(|f| f.repo.as_str()).collect();
    assert_eq!(repos.len(), 2, "both repos keep rows");
}
