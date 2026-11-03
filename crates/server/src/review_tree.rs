//! The review tree's cross-run work. Today this is the site-key matching between two runs of
//! the same scan: which of last run's sites are this run's sites, which moved and which are
//! gone. The tree types, grouping and decisions are built on top of this by later work orders.
//!
//! A decision or an edit is stored under a site key ([`camerata_api_types::site_key`]). On a
//! rerun an EXACT key match keeps its decision. A site that is no longer there under its old key
//! but has a neighbour (same repo, path and rule within
//! [`crate::ai_audit::SEMANTIC_MERGE_WINDOW`] lines) is a MOVED site: its old key and new key
//! are linked in the run's `key_links.json` so the old decision can be SHOWN beside it, stale,
//! never applied silently. A key with no match at all is GONE; the decision on it is kept for
//! history. [`match_runs`] is pure; [`RunMatch::write_key_links`] is the one place it touches
//! disk.

mod build;
mod collapse;
mod grouping;
mod mapping;
mod types;
mod unfold;

pub use build::{
    build_tree, is_thrown_out_id, kind_of, resolve_rule_categories, THROWN_OUT_SUFFIX,
};
pub use collapse::{collapse_identical, mark_located, same_place, CollapseOutcome, CollapsedRow};
pub use grouping::{group_semantic, residual_sub_id, sub_id, SubgroupAllocator};
pub use mapping::{
    family_of, is_external_tool_row, rule_node_for, RulePlacement, ToolMap, OTHER_TOOLS,
    UNCATEGORISED,
};
pub use types::{
    DuplicateLink, FlaggedBy, GroupRole, NodeKind, NodeLevel, Outlier, OutlierBy, OutlierReason,
    ReviewTree, TextSource, TreeNode,
};
pub use unfold::unfold_also_locations;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::inspections::backfill_site_keys;
use crate::onboard::Finding;
use crate::repeatability::pair_runs;

/// Name of the ledger stage that records the sites promoted to rows when an older report is
/// loaded.
pub const UNFOLD_STAGE: &str = "unfold-also-locations";

/// File name of the run's key links, beside the rest of the run's artifacts.
pub const KEY_LINKS_FILE: &str = "key_links.json";

/// How a site in the new run relates to a site in the previous run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkHow {
    /// The same key in both runs.
    Exact,
    /// A different key, found next to the old site (the code moved or the snippet changed).
    Moved,
}

/// One old site tied to one new site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyLink {
    pub old_key: String,
    pub new_key: String,
    pub how: LinkHow,
}

/// What [`match_runs`] found. Every key of the previous run is in exactly one of `exact`,
/// `moved` or `gone`; every key of the new run is in exactly one of `exact`, `moved` or `added`.
/// All lists are sorted, so the same two runs always give the same value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMatch {
    pub exact: Vec<KeyLink>,
    pub moved: Vec<KeyLink>,
    /// Keys of the previous run with no counterpart. Their decisions are kept for history.
    pub gone: Vec<String>,
    /// Keys of the new run with no counterpart.
    pub added: Vec<String>,
}

impl RunMatch {
    /// The `key_links.json` body: the moved links, each as `{old_key, new_key, how}`.
    pub fn key_links_json(&self) -> String {
        serde_json::to_string_pretty(&self.moved).unwrap_or_else(|_| "[]".to_string())
    }

    /// Write [`KEY_LINKS_FILE`] into `dir` (created if missing), atomically, and return its path.
    pub fn write_key_links(&self, dir: &Path) -> std::io::Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(KEY_LINKS_FILE);
        let tmp = dir.join(format!("{KEY_LINKS_FILE}.tmp"));
        std::fs::write(&tmp, self.key_links_json())?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }
}

/// Match the sites of a new run against the previous run's. The pairing is
/// [`crate::repeatability`]'s (the same fingerprint match, then the same neighbourhood match
/// `diff_runs` counts), applied to the rows in line order so the k-th repeat of a fingerprint
/// pairs with the k-th repeat, which is exactly when their keys' ordinals agree.
pub fn match_runs(prev: &[Finding], next: &[Finding]) -> RunMatch {
    let prev = keyed_in_line_order(prev);
    let next = keyed_in_line_order(next);
    let prev_refs: Vec<&Finding> = prev.iter().collect();
    let next_refs: Vec<&Finding> = next.iter().collect();
    let pairing = pair_runs(&prev_refs, &next_refs);

    let mut out = RunMatch::default();
    for &(p, n) in &pairing.exact {
        let link = KeyLink {
            old_key: prev[p].site_key.clone(),
            new_key: next[n].site_key.clone(),
            how: LinkHow::Exact,
        };
        // Pairs are made in line order, so the keys agree; if a caller's rows ever defeat that,
        // the pair is still honest: two different keys that were matched are a move.
        if link.old_key == link.new_key {
            out.exact.push(link);
        } else {
            out.moved.push(KeyLink {
                how: LinkHow::Moved,
                ..link
            });
        }
    }
    for &(p, n) in &pairing.by_site {
        out.moved.push(KeyLink {
            old_key: prev[p].site_key.clone(),
            new_key: next[n].site_key.clone(),
            how: LinkHow::Moved,
        });
    }
    out.gone = pairing
        .only_first
        .iter()
        .map(|&i| prev[i].site_key.clone())
        .collect();
    out.added = pairing
        .only_second
        .iter()
        .map(|&i| next[i].site_key.clone())
        .collect();
    let by_keys =
        |a: &KeyLink, b: &KeyLink| (&a.old_key, &a.new_key).cmp(&(&b.old_key, &b.new_key));
    out.exact.sort_by(by_keys);
    out.moved.sort_by(by_keys);
    out.gone.sort();
    out.added.sort();
    out
}

/// What the scan does to a repo's rows once the audit is done: collapse the rows that are
/// mechanically identical, then group what is left. Every collapsed source stays on its survivor;
/// nothing else is merged, dropped or hidden. For callers that want the whole step in one call
/// (the scan itself records the two stages in its ledger separately).
pub fn collapse_and_group(findings: Vec<Finding>, files: &[(String, String)]) -> Vec<Finding> {
    let tools = ToolMap::builtin();
    let collapsed = collapse_identical(findings, &tools);
    group_semantic(collapsed.rows, files, &tools, &mut SubgroupAllocator::new()).0
}

/// A copy of `findings` in (repo, path, line) order with every site key assigned, so the same
/// rows give the same keys and the same pairing whatever order they were stored in.
fn keyed_in_line_order(findings: &[Finding]) -> Vec<Finding> {
    let mut rows = findings.to_vec();
    backfill_site_keys(&mut rows);
    rows.sort_by(|a, b| {
        (&a.repo, &a.path, a.line, &a.site_key).cmp(&(&b.repo, &b.path, b.line, &b.site_key))
    });
    rows
}

#[cfg(test)]
mod scale_tests;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod thrown_out_tests;
#[cfg(test)]
mod tree_tests;
