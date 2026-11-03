//! Assembling the [`ReviewTree`] from a report's rows.
//!
//! The rows carry their place (`group_path`, written by [`super::grouping::group_semantic`] when
//! the scan ran). [`build_tree`] turns those paths into nodes and puts every row that has no valid
//! place (a row added after grouping, a row from an old inspection) in its rule's residual
//! sub-group, so every row is in exactly one sub-group and no row is dropped. It keeps what a
//! previous tree knew about a node that still exists (its label, cause, outliers, summary and
//! who wrote the summary), and rebuilding it from the same rows gives the same tree.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use super::grouping::residual_sub_id;
use super::mapping::{category_label, place_row, RulePlacement, ToolMap};
use super::types::{GroupRole, NodeKind, NodeLevel, Outlier, ReviewTree, TextSource, TreeNode};
use crate::ai_audit::{finding_class, primary_rank, FindingClass};
use crate::onboard::Finding;

/// The label of every rule's residual sub-group: the rows no shared cause was found for.
const RESIDUAL_LABEL: &str = "No shared cause found";

/// Whether a set of rows is read site by site or decided as a batch: a node holding a
/// security-class row or a critical or high one is an escalation node, anything else is bulk.
pub fn kind_of<'a>(mut rows: impl Iterator<Item = &'a Finding>) -> NodeKind {
    if rows.any(|f| {
        finding_class(f) == FindingClass::Security
            || matches!(f.severity.as_str(), "critical" | "high")
    }) {
        NodeKind::Escalation
    } else {
        NodeKind::Bulk
    }
}

/// The category each rule node hangs under. One rule has one parent: a rule the id alone
/// classifies has it from every row; a rule only the rows' calibration categories classify takes
/// the one most rows voted for (ties go to the first by name).
pub fn resolve_rule_categories(placements: &[RulePlacement]) -> HashMap<String, String> {
    let mut votes: BTreeMap<&str, BTreeMap<&str, usize>> = BTreeMap::new();
    for p in placements {
        *votes
            .entry(p.rule.as_str())
            .or_default()
            .entry(p.category.as_str())
            .or_default() += 1;
    }
    votes
        .into_iter()
        .map(|(rule, cats)| {
            let best = cats
                .iter()
                .max_by(|a, b| a.1.cmp(b.1).then_with(|| b.0.cmp(a.0)))
                .map_or("cat:uncategorised", |(c, _)| *c);
            (rule.to_string(), best.to_string())
        })
        .collect()
}

/// Whether `sub` is a valid sub-group id under `rule`: `sub:<rule>/<digits>`.
fn sub_belongs_to(sub: &str, rule: &str) -> bool {
    let prefix = format!("sub:{}/", rule.strip_prefix("rule:").unwrap_or(rule));
    sub.strip_prefix(&prefix)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()))
}

/// A row's `group_path` when it is a complete, well-formed place: a category node, a rule node and
/// a sub-group node of that rule.
fn valid_path(f: &Finding) -> Option<&[String; 3]> {
    let path: &[String; 3] = f.group_path.as_slice().try_into().ok()?;
    (path[0].starts_with("cat:")
        && path[1].starts_with("rule:")
        && sub_belongs_to(&path[2], &path[1]))
    .then_some(path)
}

/// The ordinal of a sub-group id (`sub:SEC-1/3` is 3).
fn sub_ordinal(sub: &str) -> u32 {
    sub.rsplit('/')
        .next()
        .and_then(|d| d.parse().ok())
        .unwrap_or(0)
}

/// Build the tree of `findings`, and write each row's `group_path` and `group_role` so they
/// agree with it. `discarded` is [`crate::onboard::ScanReport::discarded`] (A12): the rows the
/// engine threw out and kept. They are not findings and no export reads them, but they are in the
/// tree, under one `cat:<category>/thrown-out` node per category with a sub-group per reason, so
/// every row of the report is in exactly one node. Every row must have its site key (the caller assigns them first); a row
/// without one is given one here so no member is ever empty.
///
/// `prev` is the report's tree before this call: a node that is still there keeps its label,
/// cause, outliers (those whose site is still a member), summary and summary source.
pub fn build_tree(
    findings: &mut [Finding],
    discarded: &mut [Finding],
    prev: Option<ReviewTree>,
    tools: &ToolMap,
    run_id: &str,
) -> ReviewTree {
    if findings.iter().any(|f| f.site_key.is_empty()) {
        crate::inspections::backfill_site_keys(findings);
    }
    let n = findings.len();
    // A row that already has a valid place keeps it (it was placed when the scan ran, with the
    // corpus loaded); any other row is placed now and goes to its rule's residual sub-group.
    let mut placements: Vec<RulePlacement> = Vec::with_capacity(n);
    let mut row_sub: Vec<String> = Vec::with_capacity(n);
    for f in findings.iter() {
        let fresh = place_row(f, tools);
        match valid_path(f) {
            Some([category, rule, sub]) => {
                let rule_label = if fresh.rule == *rule {
                    fresh.rule_label
                } else {
                    rule.strip_prefix("rule:").unwrap_or(rule).to_string()
                };
                placements.push(RulePlacement {
                    category: category.clone(),
                    rule: rule.clone(),
                    rule_label,
                });
                row_sub.push(sub.clone());
            }
            None => {
                row_sub.push(residual_sub_id(&fresh.rule));
                placements.push(fresh);
            }
        }
    }
    let rule_category = resolve_rule_categories(&placements);

    // Members of every sub-group, in row order.
    let mut subs: BTreeMap<(String, u32, String), Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let rule = placements[i].rule.clone();
        subs.entry((rule, sub_ordinal(&row_sub[i]), row_sub[i].clone()))
            .or_default()
            .push(i);
    }

    let prev_nodes: HashMap<String, TreeNode> = prev
        .as_ref()
        .map(|t| t.nodes.iter().map(|n| (n.id.clone(), n.clone())).collect())
        .unwrap_or_default();
    let group_reviewed = prev.as_ref().is_some_and(|t| t.group_reviewed);
    let budget = prev.as_ref().map(|t| t.budget).unwrap_or_default();
    let candidates = prev
        .as_ref()
        .map(|t| t.candidates.clone())
        .unwrap_or_default();

    let mut sub_nodes: Vec<TreeNode> = Vec::new();
    for ((rule, ordinal, id), members) in &subs {
        let keys: Vec<String> = members
            .iter()
            .map(|&i| findings[i].site_key.clone())
            .collect();
        let key_set: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
        let mut rule_ids: Vec<String> = members
            .iter()
            .flat_map(|&i| {
                std::iter::once(findings[i].rule_id.clone())
                    .chain(findings[i].flagged_by.iter().map(|b| b.rule_id.clone()))
            })
            .collect();
        rule_ids.sort();
        rule_ids.dedup();
        let old = prev_nodes.get(id);
        let (label, cause) = match old {
            Some(o) => (o.label.clone(), o.cause.clone()),
            None if *ordinal == 0 => (RESIDUAL_LABEL.to_string(), None),
            None => (format!("Sub-group {ordinal}"), None),
        };
        sub_nodes.push(TreeNode {
            id: id.clone(),
            level: NodeLevel::Subgroup,
            parent: Some(rule.clone()),
            label,
            rule_ids,
            kind: kind_of(members.iter().map(|&i| &findings[i])),
            cause,
            members: keys.clone(),
            outliers: old
                .map(|o| {
                    o.outliers
                        .iter()
                        .filter(|x| key_set.contains(x.key.as_str()))
                        .cloned()
                        .collect::<Vec<Outlier>>()
                })
                .unwrap_or_default(),
            summary: old.and_then(|o| o.summary.clone()),
            summary_source: old.map_or(TextSource::Model, |o| o.summary_source),
            escalations: Vec::new(),
            flags: Vec::new(),
        });
    }

    // Rule nodes and category nodes, from the sub-groups up.
    let mut rule_nodes: BTreeMap<String, TreeNode> = BTreeMap::new();
    let mut rule_label: HashMap<&str, &str> = HashMap::new();
    for p in &placements {
        rule_label.insert(p.rule.as_str(), p.rule_label.as_str());
    }
    for s in &sub_nodes {
        let rule = s.parent.clone().unwrap_or_default();
        let category = rule_category
            .get(&rule)
            .cloned()
            .unwrap_or_else(|| "cat:uncategorised".to_string());
        let node = rule_nodes.entry(rule.clone()).or_insert_with(|| {
            let old = prev_nodes.get(&rule);
            TreeNode {
                id: rule.clone(),
                level: NodeLevel::Rule,
                parent: Some(category),
                label: rule_label
                    .get(rule.as_str())
                    .map_or_else(|| rule.clone(), |l| (*l).to_string()),
                rule_ids: Vec::new(),
                kind: NodeKind::Bulk,
                cause: None,
                members: Vec::new(),
                outliers: Vec::new(),
                summary: old.and_then(|o| o.summary.clone()),
                summary_source: old.map_or(TextSource::Model, |o| o.summary_source),
                escalations: old.map(|o| o.escalations.clone()).unwrap_or_default(),
                flags: old.map(|o| o.flags.clone()).unwrap_or_default(),
            }
        });
        node.rule_ids.extend(s.rule_ids.iter().cloned());
        if s.kind == NodeKind::Escalation {
            node.kind = NodeKind::Escalation;
        }
    }
    let new_category_node = |category: &str| {
        let old = prev_nodes.get(category);
        TreeNode {
            id: category.to_string(),
            level: NodeLevel::Category,
            parent: None,
            label: category_label(category.strip_prefix("cat:").unwrap_or(category)),
            rule_ids: Vec::new(),
            kind: NodeKind::Bulk,
            cause: None,
            members: Vec::new(),
            outliers: Vec::new(),
            summary: old.and_then(|o| o.summary.clone()),
            summary_source: old.map_or(TextSource::Model, |o| o.summary_source),
            escalations: Vec::new(),
            flags: Vec::new(),
        }
    };
    let mut category_nodes: BTreeMap<String, TreeNode> = BTreeMap::new();
    for r in rule_nodes.values_mut() {
        r.rule_ids.sort();
        r.rule_ids.dedup();
        let category = r.parent.clone().unwrap_or_default();
        let node = category_nodes
            .entry(category.clone())
            .or_insert_with(|| new_category_node(&category));
        node.rule_ids.extend(r.rule_ids.iter().cloned());
        if r.kind == NodeKind::Escalation {
            node.kind = NodeKind::Escalation;
        }
    }

    // A12: the rows the engine threw out, one node per category with a sub-group per reason.
    let thrown = thrown_out_nodes(
        discarded,
        tools,
        &rule_category,
        &prev_nodes,
        &mut category_nodes,
        &new_category_node,
    );

    for c in category_nodes.values_mut() {
        c.rule_ids.sort();
        c.rule_ids.dedup();
    }

    // Write the place of every row.
    for i in 0..n {
        let rule = placements[i].rule.clone();
        let category = rule_category
            .get(&rule)
            .cloned()
            .unwrap_or_else(|| placements[i].category.clone());
        findings[i].group_path = vec![category, rule, row_sub[i].clone()];
    }
    // The primary of each sub-group: the highest-ranked row, the earliest on a tie.
    for members in subs.values() {
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

    let mut nodes: Vec<TreeNode> = Vec::new();
    nodes.extend(category_nodes.into_values());
    nodes.extend(rule_nodes.into_values());
    nodes.extend(thrown.rule_level);
    nodes.extend(sub_nodes);
    nodes.extend(thrown.subgroups);
    ReviewTree {
        run_id: run_id.to_string(),
        engine_version: env!("CARGO_PKG_VERSION").to_string(),
        nodes,
        budget,
        candidates,
        group_reviewed,
    }
}

/// The id suffix of a category's thrown-out node.
pub const THROWN_OUT_SUFFIX: &str = "/thrown-out";

/// Whether `node_id` is a thrown-out node or one of its sub-groups.
pub fn is_thrown_out_id(node_id: &str) -> bool {
    node_id.contains(THROWN_OUT_SUFFIX)
}

/// The label of every category's thrown-out node.
const THROWN_OUT_LABEL: &str = "Thrown out by the engine";

/// The thrown-out nodes of a tree, kept apart until the tree is assembled.
struct ThrownOut {
    /// One node per category, `cat:<category>/thrown-out`.
    rule_level: Vec<TreeNode>,
    /// One sub-group per (category, reason).
    subgroups: Vec<TreeNode>,
}

/// A short, stable id fragment for a discard reason: its lowercase words joined by `-`.
fn reason_slug(reason: &str) -> String {
    let mut slug = String::new();
    for word in reason
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
    {
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word.to_ascii_lowercase());
        if slug.len() >= 40 {
            break;
        }
    }
    if slug.is_empty() {
        "no-reason".to_string()
    } else {
        slug
    }
}

/// Place every row of `discarded` (writing its `group_path` and `group_role`) and make the nodes
/// that hold them. A category that has only thrown-out rows gets its category node here.
fn thrown_out_nodes(
    discarded: &mut [Finding],
    tools: &ToolMap,
    rule_category: &HashMap<String, String>,
    prev_nodes: &HashMap<String, TreeNode>,
    category_nodes: &mut BTreeMap<String, TreeNode>,
    new_category_node: &dyn Fn(&str) -> TreeNode,
) -> ThrownOut {
    let mut out = ThrownOut {
        rule_level: Vec::new(),
        subgroups: Vec::new(),
    };
    if discarded.is_empty() {
        return out;
    }
    if discarded.iter().any(|f| f.site_key.is_empty()) {
        crate::inspections::backfill_site_keys(discarded);
    }
    // (category, reason) -> row indexes, in row order.
    let mut groups: BTreeMap<(String, String), Vec<usize>> = BTreeMap::new();
    for (i, f) in discarded.iter().enumerate() {
        let fresh = place_row(f, tools);
        let category = rule_category
            .get(&fresh.rule)
            .cloned()
            .unwrap_or(fresh.category);
        let reason = crate::engine_discard::reason_of(f)
            .unwrap_or("thrown out by the engine, no reason recorded")
            .to_string();
        groups.entry((category, reason)).or_default().push(i);
    }
    let mut slug_owner: HashMap<(String, String), String> = HashMap::new();
    let mut thrown_rule_ids: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for ((category, reason), members) in &groups {
        let cat_name = category.strip_prefix("cat:").unwrap_or(category);
        let thrown_id = format!("{category}{THROWN_OUT_SUFFIX}");
        // Two different reasons whose words slug alike keep distinct ids.
        let base = reason_slug(reason);
        let mut slug = base.clone();
        let mut n = 1;
        while slug_owner
            .get(&(category.clone(), slug.clone()))
            .is_some_and(|owner| owner != reason)
        {
            n += 1;
            slug = format!("{base}-{n}");
        }
        slug_owner.insert((category.clone(), slug.clone()), reason.clone());
        let sub_id = format!("sub:{cat_name}{THROWN_OUT_SUFFIX}/{slug}");
        let keys: Vec<String> = members
            .iter()
            .map(|&i| discarded[i].site_key.clone())
            .collect();
        let mut rule_ids: Vec<String> = members
            .iter()
            .map(|&i| discarded[i].rule_id.clone())
            .collect();
        rule_ids.sort();
        rule_ids.dedup();
        thrown_rule_ids
            .entry(category.clone())
            .or_default()
            .extend(rule_ids.iter().cloned());
        category_nodes
            .entry(category.clone())
            .or_insert_with(|| new_category_node(category))
            .rule_ids
            .extend(rule_ids.iter().cloned());
        let old = prev_nodes.get(&sub_id);
        for (k, &i) in members.iter().enumerate() {
            discarded[i].group_path = vec![category.clone(), thrown_id.clone(), sub_id.clone()];
            discarded[i].group_role = if k == 0 {
                GroupRole::Primary
            } else {
                GroupRole::Member
            };
        }
        out.subgroups.push(TreeNode {
            id: sub_id,
            level: NodeLevel::Subgroup,
            parent: Some(thrown_id),
            label: reason.clone(),
            rule_ids,
            kind: NodeKind::Bulk,
            cause: Some(reason.clone()),
            members: keys,
            outliers: Vec::new(),
            summary: old.and_then(|o| o.summary.clone()),
            summary_source: old.map_or(TextSource::Model, |o| o.summary_source),
            escalations: Vec::new(),
            flags: Vec::new(),
        });
    }
    for (category, rule_ids) in thrown_rule_ids {
        let id = format!("{category}{THROWN_OUT_SUFFIX}");
        let old = prev_nodes.get(&id);
        out.rule_level.push(TreeNode {
            id,
            level: NodeLevel::Rule,
            parent: Some(category),
            label: THROWN_OUT_LABEL.to_string(),
            rule_ids: rule_ids.into_iter().collect(),
            kind: NodeKind::Bulk,
            cause: None,
            members: Vec::new(),
            outliers: Vec::new(),
            summary: old.and_then(|o| o.summary.clone()),
            summary_source: old.map_or(TextSource::Model, |o| o.summary_source),
            escalations: Vec::new(),
            flags: Vec::new(),
        });
    }
    out
}
