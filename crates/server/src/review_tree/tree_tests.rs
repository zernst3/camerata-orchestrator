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
fn ai_row(rule_id: &str, category: Option<&str>, path: &str, line: usize, snippet: &str) -> Finding {
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
    let tree = build_tree(&mut rows, Some(prev), &tools, "run-1");
    (rows, tree)
}

/// Rows spread over files, rules and categories, with clusters, links and a lone AI row.
fn mixed_rows() -> Vec<Finding> {
    let mut rows = vec![
        row("SEC-NO-RAW-SQL-CONCAT-1", "src/db.rs", 10, "query(&format!(\"select {id}\"))"),
        row("SEC-NO-RAW-SQL-CONCAT-1", "src/db.rs", 12, "query(&format!(\"delete {id}\"))"),
        row("SEC-NO-RAW-SQL-CONCAT-1", "src/other.rs", 3, "query(&format!(\"update {id}\"))"),
        row("SEC-NO-HARDCODED-SECRETS-1", "src/cfg.rs", 4, "let key = \"sk_live_abcdef123456\";"),
        row("ARCH-MIDDLEWARE-FIRST-1", "src/app.rs", 7, "app.use(cors())"),
        ai_row("AI-CORS-ANY-ORIGIN", Some("resource-exposure"), "src/app.rs", 7, "app.use(cors())"),
        ai_row("AI-WEIRD-THING", None, "src/misc.rs", 20, "do_the_weird_thing()"),
        tool_row("camerata.security.sql-string-concat-python", "semgrep", "svc/q.py", 9, "cur.execute(\"select \" + x)"),
    ];
    for r in &mut rows {
        r.detail = format!("{} detail", r.rule_id);
    }
    rows
}

#[test]
fn grouping_never_changes_the_row_count() {
    let input = mixed_rows();
    let n = input.len();
    let (rows, _tree) = grouped(input.clone());
    assert_eq!(rows.len(), n, "every row in is a row out");
    // The same rows, in the same order, at the same severity.
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
    let member_keys: Vec<&String> = tree
        .nodes
        .iter()
        .flat_map(|n| n.members.iter())
        .collect();
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
        row("SEC-NO-RAW-SQL-CONCAT-1", "src/db.rs", 10, "query(&format!(\"select {id}\"))"),
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
    assert_eq!(native.group_path[1], tool.group_path[1], "one rule node for both");
    let rule = tree.node("rule:SEC-NO-RAW-SQL-CONCAT-1").unwrap();
    assert!(rule
        .rule_ids
        .contains(&"camerata.security.sql-string-concat-python".to_string()));
    assert!(rule.rule_ids.contains(&"SEC-NO-RAW-SQL-CONCAT-1".to_string()));

    // A tool id with no native counterpart gets its own rule node, never the native one.
    let (rows, _) = grouped(vec![tool_row("no-such-lint", "eslint", "a.js", 1, "var a = 1;")]);
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
    assert!(rule.rule_ids.contains(&"AI-ADMIN-ROUTE-NO-CHECK".to_string()));
}

#[test]
fn unmapped_row_lands_in_uncategorised_never_dropped() {
    let (rows, tree) = grouped(vec![
        // A model row with no category.
        ai_row("AI-WHO-KNOWS", None, "src/x.rs", 5, "mystery()"),
        // A model row with a category outside the closed set.
        ai_row("AI-OTHER", Some("made-up-category"), "src/y.rs", 6, "other()"),
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
        MergedLocation::site("acme/app", "src/b.rs", 20, "SEC-NO-RAW-SQL-CONCAT-1", "query(b)", false)
            .with_rating("high", "Raw SQL in b"),
        MergedLocation::site("acme/app", "src/c.rs", 30, "AI-RAW-SQL", "query(c)", true),
    ];
    let mut report = crate::onboard::build_report(
        vec!["acme/app".to_string()],
        Vec::new(),
        3,
        vec![survivor],
    );
    report.normalize_loaded();

    assert_eq!(report.findings.len(), 3, "the survivor and its two absorbed sites");
    assert!(report.findings.iter().all(|f| f.also_locations.is_empty()));
    let keys: BTreeSet<&String> = report.findings.iter().map(|f| &f.site_key).collect();
    assert_eq!(keys.len(), 3, "each row has its own site key");
    assert!(report.findings.iter().all(|f| !f.site_key.is_empty()));
    let b = report.findings.iter().find(|f| f.path == "src/b.rs").unwrap();
    assert_eq!((b.line, b.severity.as_str(), b.snippet.as_str()), (20, "high", "query(b)"));
    assert!(b.detail.contains("Raw SQL in b") && b.detail.contains("the explanation"));
    assert_eq!(b.fix_specific.as_deref(), Some("Bind the parameter."));
    let c = report.findings.iter().find(|f| f.path == "src/c.rs").unwrap();
    assert_eq!(c.rule_id, "AI-RAW-SQL");
    assert_eq!(c.severity, "medium", "a site with no recorded severity takes the survivor's");
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
    let again = build_tree(&mut rows, Some(tree.clone()), &ToolMap::builtin(), "run-1");
    assert_eq!(again, tree);
    // And a report that never had a tree gets the same one from its rows alone.
    let fresh = build_tree(&mut rows, None, &ToolMap::builtin(), "run-1");
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
    let base = row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 10, "query(&format!(\"select {id}\"))");

    // Same family, same file, same line: one row.
    let out = collapse(vec![base.clone(), row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 10, "query(&format!(\"select {id}\"))")]);
    assert_eq!(out.rows.len(), 1);

    // Within the window, one snippet contains the other: one row.
    let out = collapse(vec![
        base.clone(),
        row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 13, "query(&format!(\"select {id}\")).await"),
    ]);
    assert_eq!(out.rows.len(), 1, "a nearby line quoting the same code is the same site");

    // Within the window but the code is different: two rows.
    let out = collapse(vec![
        base.clone(),
        row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 13, "completely_different_statement()"),
    ]);
    assert_eq!(out.rows.len(), 2, "different code is a different site");

    // Beyond the window: two rows.
    let out = collapse(vec![
        base.clone(),
        row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 40, "query(&format!(\"select {id}\"))"),
    ]);
    assert_eq!(out.rows.len(), 2);

    // Another file: two rows.
    let out = collapse(vec![
        base.clone(),
        row("SEC-NO-RAW-SQL-CONCAT-1", "src/b.rs", 10, "query(&format!(\"select {id}\"))"),
    ]);
    assert_eq!(out.rows.len(), 2);

    // Another repo: two rows.
    let mut other_repo = row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 10, "query(&format!(\"select {id}\"))");
    other_repo.repo = "acme/other".to_string();
    let out = collapse(vec![base.clone(), other_repo]);
    assert_eq!(out.rows.len(), 2);

    // Another rule family at the same place: two rows (they are linked, not collapsed).
    let out = collapse(vec![
        base.clone(),
        row("SEC-NO-HARDCODED-SECRETS-1", "src/a.rs", 10, "query(&format!(\"select {id}\"))"),
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
    assert_eq!(out.rows[0].rule_id, "SEC-NO-RAW-SQL-CONCAT-1", "the floor row's text survives");
    assert_eq!(out.rows[0].flagged_by[0].tool.as_deref(), Some("semgrep"));
}

#[test]
fn collapse_keeps_every_source_key_in_flagged_by() {
    let mut floor = row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 10, "query(&format!(\"select {id}\"))");
    floor.severity = "medium".to_string();
    let mut tool = tool_row(
        "camerata.security.sql-string-concat-python",
        "semgrep",
        "src/a.rs",
        10,
        "query(&format!(\"select {id}\"))",
    );
    tool.severity = "critical".to_string();
    let mut model = ai_row("SEC-NO-RAW-SQL-CONCAT-1", Some("injection"), "src/a.rs", 12, "query(&format!(\"select {id}\")).await");
    model.severity = "high".to_string();
    let input = vec![floor, tool, model];
    let mut keyed = input.clone();
    backfill_site_keys(&mut keyed);
    let source_keys: BTreeSet<String> = keyed.iter().map(|f| f.site_key.clone()).collect();

    let out = collapse(input);
    assert_eq!(out.rows.len(), 1);
    assert_eq!(out.collapsed.len(), 2);
    assert_eq!(out.rows.len() + out.collapsed.len(), 3, "rows_in == rows_out + collapsed");
    let survivor = &out.rows[0];
    assert_eq!(survivor.severity, "critical", "the maximum severity survives");
    assert_eq!(survivor.flagged_by.len(), 2, "one entry per collapsed source");
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
    let native = row("SEC-NO-RAW-SQL-CONCAT-1", "src/a.rs", 10, "query(&format!(\"select {id}\"))");
    let model = ai_row("AI-RAW-SQL-HERE", Some("injection"), "src/a.rs", 10, "query(&format!(\"select {id}\"))");
    let out = collapse(vec![native.clone(), model.clone()]);
    assert_eq!(out.rows.len(), 2);
    assert!(out.collapsed.is_empty());
    // Even an AI row whose id merely looks like the native one.
    let lookalike = ai_row("AI-SEC-NO-RAW-SQL-CONCAT-1", Some("injection"), "src/a.rs", 10, "query(&format!(\"select {id}\"))");
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

    let one = row("SEC-NO-HARDCODED-SECRETS-1", "src/c.rs", 4, "let a = \"sk_live_AAAAAAAAAAAAAAAA1111\";");
    let two = row("SEC-NO-HARDCODED-SECRETS-1", "src/c.rs", 6, "let b = \"sk_live_BBBBBBBBBBBBBBBB2222\";");
    assert_eq!(collapse(vec![one, two]).rows.len(), 2, "two credentials stay two rows");
}

#[test]
fn link_never_changes_row_count_or_severity() {
    // Two different rule families on one line, and an AI row beside a native row.
    let input = vec![
        {
            let mut r = row("ARCH-MIDDLEWARE-FIRST-1", "src/app.rs", 7, "app.use(cors())");
            r.severity = "low".to_string();
            r
        },
        {
            let mut r = ai_row("AI-CORS-ANY-ORIGIN", Some("resource-exposure"), "src/app.rs", 7, "app.use(cors())");
            r.severity = "critical".to_string();
            r
        },
    ];
    let (rows, _) = grouped(input.clone());
    assert_eq!(rows.len(), input.len());
    for (before, after) in input.iter().zip(&rows) {
        assert_eq!(before.severity, after.severity, "a link never moves a severity");
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
        row("ARCH-MIDDLEWARE-FIRST-1", "src/app.rs", 7, "app.use(cors())"),
        ai_row("AI-CORS-ANY-ORIGIN", Some("resource-exposure"), "src/app.rs", 7, "app.use(cors())"),
    ]);
    let linked = rows.iter().find(|f| f.possible_duplicate_of.is_some()).unwrap();
    let target_key = linked.possible_duplicate_of.as_ref().unwrap().key.clone();
    let target = rows.iter().find(|f| f.site_key == target_key).unwrap();
    assert_ne!(linked.group_path[1], target.group_path[1], "the two sit in different rule nodes");
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
    assert_eq!(tree.node("rule:ARCH-STYLE-PREFERENCE-1").unwrap().kind, NodeKind::Bulk);
    assert_eq!(tree.node("rule:SEC-NO-RAW-SQL-CONCAT-1").unwrap().kind, NodeKind::Escalation);
}
