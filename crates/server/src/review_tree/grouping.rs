//! Grouping without absorption (design 3.2 and 3A.2).
//!
//! [`group_semantic`] reads a repo's rows and writes their place in the tree (`group_path`,
//! `group_role`) and the links between rows that may describe the same thing
//! (`possible_duplicate_of`). It returns EVERY row it was given, in the order it was given them:
//! it never removes, merges or hides a site, and it never changes a severity.
//!
//! The pair tests are the old merge's predicates, now only proposing placement
//! ([`crate::ai_audit::same_cause_signal`]). Nothing is quadratic: rows are bucketed by repo and
//! file first, a file with more rows than [`FULL_PAIR_CAP`] is compared inside a line window and
//! along same-rule chains, and a shared captured object is found through an inverted index (one
//! list per captured value) instead of by comparing every pair of files.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::collapse::UnionFind;
use super::mapping::{family_of, place_row, RulePlacement, ToolMap};
use super::types::{GroupRole, NodeLevel, Outlier, OutlierBy, OutlierReason, TextSource, TreeNode};
use crate::ai_audit::{
    backfill_captured_object, backfill_credential_identity, cap_placeholder_severity,
    categorize_rule_id, normalized_capture_values, objects_conflict, primary_rank,
    same_cause_signal, CauseSignal,
};
use crate::onboard::Finding;

/// A file with at most this many rows is compared pair by pair. Above it, each row is compared
/// with the [`WINDOW_FOLLOWERS`] rows after it in line order and with the next row of its rule.
const FULL_PAIR_CAP: usize = 96;
/// How many following rows (in line order) a row is compared with in a large file.
const WINDOW_FOLLOWERS: usize = 24;
/// A captured value shared by at most this many rows is compared pair by pair; a value shared by
/// more (a generic name) is compared along the chain of its rows.
const VALUE_PAIR_CAP: usize = 32;
/// A set of rows at one line this large or larger is linked to its best row only.
const SAME_LINE_PAIR_CAP: usize = 64;

/// Hands out sub-group ordinals so node ids stay unique across the repos of one scan. Ordinal 0
/// is the residual sub-group of every rule (the rows with no shared cause found).
#[derive(Debug, Clone, Default)]
pub struct SubgroupAllocator {
    next: BTreeMap<String, u32>,
}

impl SubgroupAllocator {
    /// A fresh allocator.
    pub fn new() -> Self {
        Self::default()
    }

    /// The next sub-group id under `rule_node_id` (`rule:SEC-RLS-1` gives `sub:SEC-RLS-1/1`,
    /// then `/2`).
    pub fn next_id(&mut self, rule_node_id: &str) -> String {
        let n = self.next.entry(rule_node_id.to_string()).or_insert(0);
        *n += 1;
        sub_id(rule_node_id, *n)
    }
}

/// The sub-group node id of ordinal `n` under a rule node.
pub fn sub_id(rule_node_id: &str, n: u32) -> String {
    format!(
        "sub:{}/{n}",
        rule_node_id.strip_prefix("rule:").unwrap_or(rule_node_id)
    )
}

/// The residual sub-group id of a rule node.
pub fn residual_sub_id(rule_node_id: &str) -> String {
    sub_id(rule_node_id, 0)
}

/// What every row gets before any pair is tested: a placeholder secret is held at its ceiling, a
/// row that reached here unredacted (carried forward from an older cache) is redacted, and the
/// category and captured-object backfills the same-cause test reads are made.
pub(super) fn prepare_row(f: &mut Finding) {
    cap_placeholder_severity(f);
    crate::onboard::redact::redact_finding(f);
    if f.category.is_none() {
        f.category = categorize_rule_id(&f.rule_id);
    }
    backfill_captured_object(f);
    backfill_credential_identity(f);
}

/// One pair the same-cause test accepted.
pub(super) struct Edge {
    a: usize,
    b: usize,
    signal: CauseSignal,
}

/// Place every row of `findings` in the tree and link the rows that may be duplicates. Returns
/// the rows (same count, same order) and the sub-group nodes it formed; a rule's residual
/// sub-group is not returned (the tree assembly makes it from the rows left in it).
///
/// `files` is the repo's `(path, content)` list: the same-construct test reads the file around
/// the two lines. Rows of several repos are fine; they are never compared with each other.
pub fn group_semantic(
    mut findings: Vec<Finding>,
    files: &[(String, String)],
    tools: &ToolMap,
    alloc: &mut SubgroupAllocator,
) -> (Vec<Finding>, Vec<TreeNode>) {
    for f in &mut findings {
        prepare_row(f);
    }
    crate::inspections::backfill_site_keys(&mut findings);
    let n = findings.len();

    let content_of: HashMap<&str, &str> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let placements: Vec<RulePlacement> = findings.iter().map(|f| place_row(f, tools)).collect();
    let rule_category = super::build::resolve_rule_categories(&placements);
    let families: Vec<String> = findings.iter().map(|f| family_of(f, tools)).collect();

    let (component_of, edges) = cluster_rows(&findings, &content_of);

    // Components and the cause that holds each together.
    let mut cause_of_component: HashMap<usize, CauseSignal> = HashMap::new();
    for e in &edges {
        let root = component_of[e.a];
        let entry = cause_of_component.entry(root).or_insert(e.signal);
        if e.signal < *entry {
            *entry = e.signal;
        }
    }

    // Sub-groups: the members of one component that share a rule node, when there are two or
    // more of them. Everything else is the rule's residual sub-group.
    let mut rule_ids_sorted: Vec<&str> = placements.iter().map(|p| p.rule.as_str()).collect();
    rule_ids_sorted.sort_unstable();
    rule_ids_sorted.dedup();
    let mut members_by_rule_component: BTreeMap<(&str, usize), Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        members_by_rule_component
            .entry((placements[i].rule.as_str(), component_of[i]))
            .or_default()
            .push(i);
    }
    // Rule by rule, component by lowest member: deterministic ordinals.
    let mut sub_of_row: Vec<Option<String>> = vec![None; n];
    let mut sub_nodes: Vec<TreeNode> = Vec::new();
    for ((rule, root), members) in &members_by_rule_component {
        if members.len() < 2 {
            continue;
        }
        let id = alloc.next_id(rule);
        for &i in members {
            sub_of_row[i] = Some(id.clone());
        }
        let cause = cause_of_component
            .get(root)
            .map_or("a shared cause", |s| s.cause());
        let primary = *members
            .iter()
            .max_by_key(|&&i| primary_rank(&findings[i], n, i))
            .unwrap_or(&members[0]);
        let outliers: Vec<Outlier> = members
            .iter()
            .copied()
            .filter(|&i| i != primary && objects_conflict(&findings[i], &findings[primary]))
            .map(|i| Outlier {
                key: findings[i].site_key.clone(),
                reason: OutlierReason::DifferentObject,
                by: OutlierBy::Engine,
            })
            .collect();
        let mut rule_ids: Vec<String> = members
            .iter()
            .flat_map(|&i| {
                std::iter::once(findings[i].rule_id.clone())
                    .chain(findings[i].flagged_by.iter().map(|b| b.rule_id.clone()))
            })
            .collect();
        rule_ids.sort();
        rule_ids.dedup();
        sub_nodes.push(TreeNode {
            id,
            level: NodeLevel::Subgroup,
            parent: Some((*rule).to_string()),
            label: format!("Same cause: {cause}"),
            rule_ids,
            kind: super::build::kind_of(members.iter().map(|&i| &findings[i])),
            cause: Some(cause.to_string()),
            members: members
                .iter()
                .map(|&i| findings[i].site_key.clone())
                .collect(),
            outliers,
            summary: None,
            summary_source: TextSource::Model,
        });
    }

    // Write the place of every row, and its role inside its sub-group.
    let mut groups: BTreeMap<(usize, String), Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let rule = &placements[i].rule;
        let sub = sub_of_row[i]
            .clone()
            .unwrap_or_else(|| residual_sub_id(rule));
        let category = rule_category
            .get(rule)
            .cloned()
            .unwrap_or_else(|| placements[i].category.clone());
        findings[i].group_path = vec![category, rule.clone(), sub.clone()];
        let rule_index = rule_ids_sorted.binary_search(&rule.as_str()).unwrap_or(0);
        groups.entry((rule_index, sub)).or_default().push(i);
    }
    for members in groups.values() {
        let primary = members
            .iter()
            .copied()
            .max_by_key(|&i| primary_rank(&findings[i], n, i))
            .unwrap_or(members[0]);
        for &i in members {
            findings[i].group_role = if i == primary {
                GroupRole::Primary
            } else {
                GroupRole::Member
            };
        }
    }

    write_links(&mut findings, &edges, &families);
    (findings, sub_nodes)
}

/// The pair tests: which rows look like they share a cause. Returns each row's component (the
/// lowest row index connected to it) and the pairs that connected them. Rows are bucketed by
/// repo and file first and rows of different repos are never compared; see the module comment
/// for the caps that keep it linear.
pub(super) fn cluster_rows(
    findings: &[Finding],
    content_of: &HashMap<&str, &str>,
) -> (Vec<usize>, Vec<Edge>) {
    let n = findings.len();
    // Candidate pairs, repo by repo.
    let mut by_repo: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, f) in findings.iter().enumerate() {
        by_repo.entry(f.repo.as_str()).or_default().push(i);
    }
    let mut uf = UnionFind::new(n);
    let mut edges: Vec<Edge> = Vec::new();
    let mut tested: HashSet<(usize, usize)> = HashSet::new();
    let mut test =
        |a: usize, b: usize, findings: &[Finding], uf: &mut UnionFind, edges: &mut Vec<Edge>| {
            let (a, b) = if a < b { (a, b) } else { (b, a) };
            if a == b || !tested.insert((a, b)) {
                return;
            }
            let content = content_of.get(findings[a].path.as_str()).copied();
            if let Some(signal) = same_cause_signal(&findings[a], &findings[b], content) {
                uf.union(a, b);
                edges.push(Edge { a, b, signal });
            }
        };
    for rows in by_repo.values() {
        // Rows of one file.
        let mut by_path: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for &i in rows {
            by_path
                .entry(findings[i].path.as_str())
                .or_default()
                .push(i);
        }
        for members in by_path.values_mut() {
            if members.len() <= FULL_PAIR_CAP {
                for (k, &a) in members.iter().enumerate() {
                    for &b in &members[k + 1..] {
                        test(a, b, findings, &mut uf, &mut edges);
                    }
                }
            } else {
                members.sort_by_key(|&i| (findings[i].line, i));
                for (k, &a) in members.iter().enumerate() {
                    for &b in members[k + 1..].iter().take(WINDOW_FOLLOWERS) {
                        test(a, b, findings, &mut uf, &mut edges);
                    }
                }
                let mut last_of_rule: HashMap<&str, usize> = HashMap::new();
                for &i in members.iter() {
                    if let Some(prev) = last_of_rule.insert(findings[i].rule_id.as_str(), i) {
                        test(prev, i, findings, &mut uf, &mut edges);
                    }
                }
            }
        }
        // Rows of different files that name the same captured object.
        let mut by_value: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for &i in rows {
            let mut values: Vec<String> = normalized_capture_values(&findings[i])
                .into_iter()
                .collect();
            values.sort();
            for v in values {
                by_value.entry(v).or_default().push(i);
            }
        }
        for members in by_value.values() {
            if members.len() < 2 {
                continue;
            }
            if members.len() <= VALUE_PAIR_CAP {
                for (k, &a) in members.iter().enumerate() {
                    for &b in &members[k + 1..] {
                        test(a, b, findings, &mut uf, &mut edges);
                    }
                }
            } else {
                for pair in members.windows(2) {
                    test(pair[0], pair[1], findings, &mut uf, &mut edges);
                }
            }
        }
    }

    let component_of: Vec<usize> = (0..n).map(|i| uf.find(i)).collect();
    (component_of, edges)
}

/// Write `possible_duplicate_of` on the lower-ranked row of every pair that looks similar but is
/// not mechanically identical: different rule families on one line, rows of different families
/// the same-cause test paired, and the same captured object at two sites of one file. A row gets
/// at most one link, to the highest-ranked row it may duplicate. Rank is the sub-group primary
/// ranking, so the row the engine would show first is the one the others point at.
fn write_links(findings: &mut [Finding], edges: &[Edge], families: &[String]) {
    let n = findings.len();
    // (hint priority, rank of the target, target index, hint) - lowest priority number wins,
    // then the highest-ranked target.
    type Best = (
        u8,
        std::cmp::Reverse<(u8, u8, u8, u8, u8, u8, u8, usize)>,
        usize,
        String,
    );
    let mut best: Vec<Option<Best>> = vec![None; n];
    let rank = |findings: &[Finding], i: usize| primary_rank(&findings[i], n, i);
    let mut offer = |findings: &[Finding], row: usize, target: usize, priority: u8, hint: &str| {
        // Only the lower-ranked row of a pair carries the link.
        if rank(findings, target) <= rank(findings, row) {
            return;
        }
        let candidate: Best = (
            priority,
            std::cmp::Reverse(rank(findings, target)),
            target,
            hint.to_string(),
        );
        let replace = match &best[row] {
            None => true,
            Some(current) => (candidate.0, &candidate.1) < (current.0, &current.1),
        };
        if replace {
            best[row] = Some(candidate);
        }
    };

    // Different rule families at one place.
    let mut same_line: HashMap<(&str, &str, usize), Vec<usize>> = HashMap::new();
    for (i, f) in findings.iter().enumerate() {
        if f.line != 0 {
            same_line
                .entry((f.repo.as_str(), f.path.as_str(), f.line))
                .or_default()
                .push(i);
        }
    }
    for rows in same_line.values() {
        if rows.len() < 2 {
            continue;
        }
        if rows.len() >= SAME_LINE_PAIR_CAP {
            let top = rows
                .iter()
                .copied()
                .max_by_key(|&i| rank(findings, i))
                .unwrap_or(rows[0]);
            for &i in rows {
                if families[i] != families[top] {
                    offer(findings, i, top, 0, "same_line");
                }
            }
            continue;
        }
        for &a in rows {
            for &b in rows {
                if a != b && families[a] != families[b] {
                    offer(findings, a, b, 0, "same_line");
                }
            }
        }
    }
    // Pairs the same-cause test accepted that are not one family.
    for e in edges {
        if families[e.a] != families[e.b] {
            offer(findings, e.a, e.b, 1, e.signal.hint());
            offer(findings, e.b, e.a, 1, e.signal.hint());
        } else if e.signal == CauseSignal::SharedObject && findings[e.a].path == findings[e.b].path
        {
            offer(findings, e.a, e.b, 2, "same_object_same_file");
            offer(findings, e.b, e.a, 2, "same_object_same_file");
        }
    }
    let keys: Vec<String> = findings.iter().map(|f| f.site_key.clone()).collect();
    for (i, b) in best.into_iter().enumerate() {
        findings[i].possible_duplicate_of =
            b.map(|(_, _, target, hint)| super::types::DuplicateLink {
                key: keys[target].clone(),
                hint,
            });
    }
}
