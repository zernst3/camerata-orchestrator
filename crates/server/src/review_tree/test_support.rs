//! Helpers the older merge tests use to ask the questions they were written to ask, now that
//! nothing merges: which rows does the same-cause test cluster, which row would head the cluster,
//! and what does the collapse leave. They are test-only; the engine never calls them.

use std::collections::HashMap;

use super::collapse::{collapse_identical, mark_located};
use super::grouping::cluster_rows;
use super::mapping::ToolMap;
use crate::ai_audit::primary_rank;
use crate::onboard::Finding;

/// The clusters the same-cause test forms over `findings` (after the category and capture
/// backfills the grouping pass makes), each as its rows in input order; clusters in order of
/// their first row. No row is merged, dropped or changed beyond the backfills.
pub(crate) fn clusters(
    mut findings: Vec<Finding>,
    files: &[(String, String)],
) -> Vec<Vec<Finding>> {
    for f in &mut findings {
        super::grouping::prepare_row(f);
    }
    let content_of: HashMap<&str, &str> = files
        .iter()
        .map(|(p, c)| (p.as_str(), c.as_str()))
        .collect();
    let (component_of, _edges) = cluster_rows(&findings, &content_of);
    let mut order: Vec<usize> = Vec::new();
    let mut by_root: HashMap<usize, Vec<Finding>> = HashMap::new();
    for (i, f) in findings.into_iter().enumerate() {
        let root = component_of[i];
        if !by_root.contains_key(&root) {
            order.push(root);
        }
        by_root.entry(root).or_default().push(f);
    }
    order
        .into_iter()
        .filter_map(|r| by_root.remove(&r))
        .collect()
}

/// The row that heads a cluster: the highest-ranked, the earliest on a tie (the sub-group
/// primary).
pub(crate) fn head_of(group: &[Finding]) -> &Finding {
    let n = group.len();
    group
        .iter()
        .enumerate()
        .max_by_key(|(i, f)| primary_rank(f, n, *i))
        .map(|(_, f)| f)
        .expect("a cluster has a row")
}

/// The row that heads a group of rows, as an owned row (the sub-group primary).
pub(crate) fn head_row(group: Vec<Finding>) -> Finding {
    head_of(&group).clone()
}

/// One row per cluster: the row that heads it, untouched.
pub(crate) fn cluster_heads(findings: Vec<Finding>, files: &[(String, String)]) -> Vec<Finding> {
    clusters(findings, files)
        .iter()
        .map(|g| head_of(g).clone())
        .collect()
}

/// Whether the same-cause test pairs two rows, run on its own.
pub(crate) fn same_cause_hint(a: &Finding, b: &Finding, content: Option<&str>) -> bool {
    crate::ai_audit::same_cause_signal(a, b, content).is_some()
}

/// What the collapse leaves of `findings` after the located flag is set from `files`.
pub(crate) fn collapse_located(
    mut findings: Vec<Finding>,
    files: &[(String, String)],
) -> Vec<Finding> {
    mark_located(&mut findings, files);
    collapse_identical(findings, &ToolMap::builtin()).rows
}

/// The rule ids of a cluster's rows.
pub(crate) fn cluster_rule_ids(group: &[Finding]) -> std::collections::BTreeSet<String> {
    group.iter().map(|f| f.rule_id.clone()).collect()
}

/// The lines of a cluster's rows.
pub(crate) fn cluster_lines(group: &[Finding]) -> std::collections::BTreeSet<usize> {
    group.iter().map(|f| f.line).collect()
}

/// Every possible-duplicate link in `rows` as `(rule id of the row, rule id of its target, hint)`,
/// sorted.
pub(crate) fn link_pairs(rows: &[Finding]) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = rows
        .iter()
        .filter_map(|f| {
            let link = f.possible_duplicate_of.as_ref()?;
            let target = rows.iter().find(|t| t.site_key == link.key)?;
            Some((f.rule_id.clone(), target.rule_id.clone(), link.hint.clone()))
        })
        .collect();
    out.sort();
    out
}
