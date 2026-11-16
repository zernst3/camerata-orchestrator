//! A style linter's rule is one decision per repository, never one decision per place.
//!
//! Every place a style linter (ESLint, Clippy, Ruff) flags under one rule in one repository is
//! one rule group and one Fix: the operator decides it once ("fix it everywhere" or "rule it out
//! everywhere") and the decision is written to every place. Nothing hides: every place stays a
//! row. A linter's security checks (an ESLint `security/` rule, a Ruff `S` rule) keep their
//! per-place handling. Synthetic fixtures only.

use std::collections::BTreeSet;

use camerata_api_types::audit::FindingKind;
use camerata_api_types::decisions::{DecisionState, NewDecisionReq, Scope};
use camerata_api_types::review::EscalationReason;

use super::*;
use crate::decisions::{apply_to, DecisionCtx, DecisionStore};
use crate::inspections::backfill_site_keys;
use crate::onboard::Finding;

/// One place a linter flagged, the way `scan_tools` writes a preview row. Places are spread over
/// files (ten lines per file) so the same-cause test has neighbours to pair.
pub(super) fn lint_row(repo: &str, tool: &str, rule: &str, i: usize) -> Finding {
    Finding {
        repo: repo.to_string(),
        path: format!("src/mod{}/file{}.js", i % 7, i / 10),
        line: 1 + (i % 10) * 3,
        rule_id: rule.to_string(),
        severity: "medium".to_string(),
        snippet: format!("var item{i} = load({});", i % 5),
        detail: format!(
            "Unexpected var, use let or const instead. (found by {tool} during this scan; not yet \
             part of the CI pipeline)."
        ),
        status: "suppressed-baseline".to_string(),
        preview: true,
        preview_tool: Some(tool.to_string()),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

/// `n` places of one lint rule in one repository.
pub(super) fn lint_rows(repo: &str, tool: &str, rule: &str, n: usize) -> Vec<Finding> {
    (0..n).map(|i| lint_row(repo, tool, rule, i)).collect()
}

/// A native security row (not a lint), at a place of its own choosing.
pub(super) fn native_row(repo: &str, path: &str, line: usize) -> Finding {
    Finding {
        repo: repo.to_string(),
        path: path.to_string(),
        line,
        rule_id: "SEC-NO-RAW-SQL-CONCAT-1".to_string(),
        severity: "high".to_string(),
        snippet: format!("db.query(\"select \" + input{line})"),
        detail: "The query is built from a request value.".to_string(),
        kind: Some(FindingKind::Defect),
        ..Finding::default()
    }
}

/// Group `rows` the way the scan does (one repository at a time, one allocator for the scan) and
/// assemble the tree the way sealing does.
pub(super) fn grouped(rows: Vec<Finding>) -> (Vec<Finding>, ReviewTree) {
    grouped_with(rows, &ToolMap::builtin())
}

/// [`grouped`] with the scan's tool map (the corpus's selectors) for the grouping pass; the tree
/// is assembled with the built-in tables, as the seal and the group review do.
pub(super) fn grouped_with(rows: Vec<Finding>, tools: &ToolMap) -> (Vec<Finding>, ReviewTree) {
    let mut alloc = SubgroupAllocator::new();
    let mut repos: Vec<String> = rows.iter().map(|f| f.repo.clone()).collect();
    repos.sort();
    repos.dedup();
    let mut all: Vec<Finding> = Vec::new();
    let mut sub_nodes: Vec<TreeNode> = Vec::new();
    for repo in repos {
        let of_repo: Vec<Finding> = rows.iter().filter(|f| f.repo == repo).cloned().collect();
        let (out, subs) = group_semantic(of_repo, &[], tools, &mut alloc);
        all.extend(out);
        sub_nodes.extend(subs);
    }
    let prev = ReviewTree {
        nodes: sub_nodes,
        ..ReviewTree::default()
    };
    backfill_site_keys(&mut all);
    let tree = build_tree(&mut all, &mut [], Some(prev), &ToolMap::builtin(), "run-1");
    (all, tree)
}

/// The group reviewer split a rule group: the first `n` rows of `rule` are moved to a sub-group of
/// its own, and the tree is assembled again the way the group review does.
pub(super) fn reviewer_splits(
    rows: &mut [Finding],
    tree: ReviewTree,
    rule: &str,
    n: usize,
) -> ReviewTree {
    for f in rows.iter_mut().filter(|f| f.rule_id == rule).take(n) {
        let sub = format!(
            "{}/7",
            f.group_path[2]
                .rsplit_once('/')
                .map_or("", |(head, _)| head)
        );
        f.group_path[2] = sub;
    }
    build_tree(rows, &mut [], Some(tree), &ToolMap::builtin(), "run-1")
}

/// The rule groups (sub-group nodes) that hold a row of `rule` in `repo`.
pub(super) fn rule_groups_of<'a>(
    tree: &'a ReviewTree,
    rows: &[Finding],
    rule: &str,
    repo: &str,
) -> Vec<&'a TreeNode> {
    let keys: BTreeSet<&str> = rows
        .iter()
        .filter(|f| f.rule_id == rule && f.repo == repo)
        .map(|f| f.site_key.as_str())
        .collect();
    tree.nodes
        .iter()
        .filter(|n| n.level == NodeLevel::Subgroup)
        .filter(|n| n.members.iter().any(|k| keys.contains(k.as_str())))
        .collect()
}

/// The Fixes that cover a row of `rule` in `repo`.
pub(super) fn fixes_of<'a>(
    tree: &'a ReviewTree,
    rows: &[Finding],
    rule: &str,
    repo: &str,
) -> Vec<&'a FixNode> {
    let subs: BTreeSet<&str> = rule_groups_of(tree, rows, rule, repo)
        .into_iter()
        .map(|n| n.id.as_str())
        .collect();
    tree.fixes
        .iter()
        .filter(|fx| fx.members.iter().any(|m| subs.contains(m.as_str())))
        .collect()
}

pub(super) fn keys_of(rows: &[Finding], rule: &str, repo: &str) -> Vec<String> {
    let mut keys: Vec<String> = rows
        .iter()
        .filter(|f| f.rule_id == rule && f.repo == repo)
        .map(|f| f.site_key.clone())
        .collect();
    keys.sort();
    keys
}

#[test]
fn five_hundred_places_of_one_style_lint_rule_are_one_rule_group_and_one_fix() {
    let (mut rows, tree) = grouped(lint_rows("acme/web", "eslint", "no-var", 500));
    // The group reviewer splits the rule group, as it may for any rule. A style rule stays one.
    let tree = reviewer_splits(&mut rows, tree, "no-var", 120);

    let groups = rule_groups_of(&tree, &rows, "no-var", "acme/web");
    assert_eq!(
        groups.len(),
        1,
        "500 places of one style rule are one rule group, got {:?}",
        groups.iter().map(|n| &n.id).collect::<Vec<_>>()
    );
    let group = groups[0];
    assert_eq!(group.members.len(), 500, "the rule group holds every place");
    assert_eq!(
        group.kind,
        NodeKind::Bulk,
        "a style rule's group is decided as a batch"
    );
    let fixes = fixes_of(&tree, &rows, "no-var", "acme/web");
    assert_eq!(fixes.len(), 1, "one rule is one Fix: one item to decide");
    assert_eq!(fixes[0].members, vec![group.id.clone()]);
    assert_eq!(fixes[0].places.len(), 500, "the Fix lists every place");
    // Every row is still a row, each in exactly one node.
    assert_eq!(rows.len(), 500);
    assert!(rows.iter().all(|f| f.group_path[2] == group.id));
}

#[test]
fn a_decision_on_the_lint_group_applies_to_every_place_and_is_recorded_once() {
    let mut input = lint_rows("acme/web", "eslint", "no-var", 500);
    // A non-lint problem at the same place as one lint place: it keeps its own decision.
    let shared = lint_row("acme/web", "eslint", "no-var", 3);
    input.push(native_row("acme/web", &shared.path, shared.line));
    let (mut rows, tree) = grouped(input);
    let tree = reviewer_splits(&mut rows, tree, "no-var", 120);

    // The operator decides the one item the rule makes: the group of any of its places.
    let first = rows.iter().find(|f| f.rule_id == "no-var").unwrap();
    let node_id = first.group_path[2].clone();
    let members = tree.batch_members(&node_id);
    let store = DecisionStore::new();
    let req = NewDecisionReq {
        scope: Scope::Node(node_id.clone()),
        key: String::new(),
        state: DecisionState::FalsePositive,
        reason: "The team allows var in this legacy bundle.".to_string(),
        decided_by: "auditor".to_string(),
        needed_me: None,
        sample_verdict: None,
        except: Vec::new(),
        run_id: String::new(),
        decided_at: String::new(),
    };
    let ctx = DecisionCtx {
        run_id: "run-1",
        engine_version: "0.0.0-test",
        now: "2026-11-16T09:00:00Z",
    };
    let (record, written) = store
        .record_node("p", &node_id, &members, &req, ctx)
        .expect("the decision is recorded");

    let records = store.active("p");
    let node_records: Vec<_> = records
        .iter()
        .filter(|d| matches!(d.scope, Scope::Node(_)))
        .collect();
    assert_eq!(node_records.len(), 1, "the decision is recorded once");
    assert_eq!(
        record.applies_to.len(),
        500,
        "recorded once with the count of places"
    );
    assert_eq!(written, 500);
    let applied = apply_to(&tree, &records, &[]);
    for key in keys_of(&rows, "no-var", "acme/web") {
        let standing = applied.get(&key).expect("every place has a standing");
        let d = standing
            .decision
            .as_ref()
            .unwrap_or_else(|| panic!("{key:?} is decided by the group"));
        assert_eq!(d.state, DecisionState::FalsePositive);
        assert_eq!(d.inherited_from.as_deref(), Some(record.id.as_str()));
    }
    // The SQL problem at the shared place is not covered: it needs its own decision.
    let sql = rows
        .iter()
        .find(|f| f.rule_id == "SEC-NO-RAW-SQL-CONCAT-1")
        .unwrap();
    assert!(!members.contains(&sql.site_key));
    assert!(applied
        .get(&sql.site_key)
        .is_none_or(|s| s.decision.is_none()));
}

#[test]
fn security_family_lints_keep_per_place_handling() {
    // The corpus maps the two security checks to rules that are not security rules, so only the
    // lint's own id says what it is: nothing else would keep them per place.
    let selectors: std::collections::HashMap<String, Vec<String>> = [
        (
            "JAVASCRIPT-OBJECT-ACCESS-1",
            "security/detect-object-injection",
        ),
        ("PYTHON-TESTING-ASSERTS-1", "s101"),
    ]
    .into_iter()
    .map(|(native, lint)| (native.to_string(), vec![lint.to_string()]))
    .collect();
    let tools = ToolMap::from_selectors(Some(&selectors));
    let mut input = Vec::new();
    for repo in ["acme/web", "acme/admin"] {
        input.extend(lint_rows(
            repo,
            "eslint",
            "security/detect-object-injection",
            4,
        ));
        input.extend(lint_rows(repo, "ruff", "S101", 4));
        input.extend(lint_rows(repo, "ruff", "S608", 4));
        input.extend(lint_rows(repo, "eslint", "no-var", 4));
    }
    let (mut rows, mut tree) = grouped_with(input, &tools);
    // The group reviewer splits every rule group it reads.
    for rule in ["security/detect-object-injection", "S101", "S608", "no-var"] {
        tree = reviewer_splits(&mut rows, tree, rule, 2);
    }

    // A style rule stays one group per repository whatever the split...
    let web = rule_groups_of(&tree, &rows, "no-var", "acme/web");
    let admin = rule_groups_of(&tree, &rows, "no-var", "acme/admin");
    assert_eq!((web.len(), admin.len()), (1, 1));
    assert_ne!(web[0].id, admin[0].id, "two repositories, two rule groups");
    // ...a security lint keeps today's handling in every repository: no group that holds one is a
    // lint group, and the reviewer's split stands, place by place.
    for rule in ["security/detect-object-injection", "S101", "S608"] {
        for repo in ["acme/web", "acme/admin"] {
            let groups = rule_groups_of(&tree, &rows, rule, repo);
            assert!(!groups.is_empty(), "{rule} in {repo} has a group");
            for g in &groups {
                assert!(
                    !is_lint_sub_id(&g.id),
                    "{rule} in {repo} is in the lint group {}",
                    g.id
                );
            }
            for f in rows.iter().filter(|f| f.rule_id == rule && f.repo == repo) {
                assert!(!in_lint_group(f), "{rule} in {repo}: {:?}", f.group_path);
            }
        }
        let groups: BTreeSet<&str> = ["acme/web", "acme/admin"]
            .into_iter()
            .flat_map(|repo| rule_groups_of(&tree, &rows, rule, repo))
            .map(|n| n.id.as_str())
            .collect();
        assert!(
            groups.len() >= 2,
            "{rule}: the reviewer's split stands: {groups:?}"
        );
    }
    // And a security lint's place can still be escalated, one place at a time.
    let s608 = rows.iter().find(|f| f.rule_id == "S608").unwrap().clone();
    let rule_node = s608.group_path[1].clone();
    tree.nodes
        .iter_mut()
        .find(|n| n.id == rule_node)
        .unwrap()
        .escalations
        .push(NodeEscalation {
            key: s608.site_key.clone(),
            reason: EscalationReason::Injection,
            rank: 1,
            why: "SQL is built from a request value.".to_string(),
        });
    let ranked = rank_candidates(&tree.nodes, &rows, &RankSettings::default());
    assert!(ranked.candidates.iter().any(|c| c.key == s608.site_key));
}

#[test]
fn two_repositories_with_the_same_lint_rule_make_two_groups() {
    let mut input = lint_rows("acme/web", "eslint", "eqeqeq", 30);
    input.extend(lint_rows("acme/admin", "eslint", "eqeqeq", 20));
    let (rows, tree) = grouped(input);
    let web = rule_groups_of(&tree, &rows, "eqeqeq", "acme/web");
    let admin = rule_groups_of(&tree, &rows, "eqeqeq", "acme/admin");
    assert_eq!(web.len(), 1);
    assert_eq!(admin.len(), 1);
    assert_ne!(web[0].id, admin[0].id, "one rule group per repository");
    assert_eq!(web[0].members.len(), 30);
    assert_eq!(admin[0].members.len(), 20);
    assert_eq!(fixes_of(&tree, &rows, "eqeqeq", "acme/web").len(), 1);
    assert_eq!(fixes_of(&tree, &rows, "eqeqeq", "acme/admin").len(), 1);
    assert_ne!(
        fixes_of(&tree, &rows, "eqeqeq", "acme/web")[0].id,
        fixes_of(&tree, &rows, "eqeqeq", "acme/admin")[0].id
    );
}

#[test]
fn clippy_and_ruff_style_rules_are_grouped_like_eslint() {
    let mut input = lint_rows("acme/core", "clippy", "clippy::needless_return", 12);
    input.extend(lint_rows("acme/core", "ruff", "E501", 12));
    let (mut rows, tree) = grouped(input);
    let tree = reviewer_splits(&mut rows, tree, "E501", 5);
    let tree = reviewer_splits(&mut rows, tree, "clippy::needless_return", 5);
    for rule in ["clippy::needless_return", "E501"] {
        assert_eq!(
            rule_groups_of(&tree, &rows, rule, "acme/core").len(),
            1,
            "{rule} is one rule group"
        );
        assert_eq!(fixes_of(&tree, &rows, rule, "acme/core").len(), 1);
    }
}
