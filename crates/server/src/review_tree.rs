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
    build_tree, engine_outlier_reasons, is_thrown_out_id, kind_of, resolve_rule_categories,
    unplaced_summary, THROWN_OUT_SUFFIX,
};
pub use collapse::{collapse_identical, mark_located, same_place, CollapseOutcome, CollapsedRow};
pub use grouping::{group_semantic, residual_sub_id, sub_id, SubgroupAllocator};
pub use mapping::{
    family_of, is_external_tool_row, rule_node_for, RulePlacement, ToolMap, OTHER_TOOLS,
    UNCATEGORISED,
};
pub use types::{
    DuplicateLink, FlaggedBy, GroupRole, NodeCounts, NodeEscalation, NodeKind, NodeLevel, Outlier,
    OutlierBy, OutlierReason, ReviewTree, TextSource, TreeNode,
};
pub use unfold::unfold_also_locations;

use std::path::{Path, PathBuf};

use camerata_api_types::review::{EscalationBudget, EscalationCandidate, EscalationReason};

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

// ── Ranking (design 4.4) ───────────────────────────────────────────────────────────────────

/// What the ranking needs besides the tree: the project's escalation settings and the lines the
/// scan counted.
#[derive(Debug, Clone, PartialEq)]
pub struct RankSettings {
    pub rate_per_kloc: f32,
    pub floor: usize,
    pub counted_lines: usize,
    /// The reasons that may exceed the budget (at critical severity, `evidence_unverified` at
    /// any).
    pub over_budget_reasons: Vec<EscalationReason>,
}

impl Default for RankSettings {
    fn default() -> Self {
        Self {
            rate_per_kloc: camerata_api_types::review::DEFAULT_ESCALATION_RATE_PER_KLOC,
            floor: camerata_api_types::review::DEFAULT_ESCALATION_FLOOR,
            counted_lines: 0,
            over_budget_reasons: EscalationReason::default_over_budget_set(),
        }
    }
}

/// The ranked candidates and the budget they were marked against.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub candidates: Vec<EscalationCandidate>,
    pub budget: EscalationBudget,
}

/// The group reviewer's rank 0 means "not ranked by the group reviewer" (an escalation the engine
/// added); it sorts after every ranked one.
fn rank_sort_key(stage2_rank: u32) -> u32 {
    if stage2_rank == 0 {
        u32::MAX
    } else {
        stage2_rank
    }
}

/// One row that was escalated, before it is placed in a site.
struct RawEscalation {
    node_id: String,
    key: String,
    reason: EscalationReason,
    stage2_rank: u32,
    severity: String,
    why: String,
    site: (String, String, usize),
}

/// One site (repo, path and line) with every escalated row at it: one slot of the budget (R-1).
struct RankedSite {
    /// The rows at the site, the best reason first.
    members: Vec<RawEscalation>,
    /// The highest severity of any row at the site.
    severity: String,
    /// The best reason rank of any row at the site.
    reason_rank: u8,
    /// The best group reviewer rank of any row at the site (0 sorts last).
    stage2_rank: u32,
    /// The node of the row that decides the site's place.
    node_id: String,
    /// The site's first key, for a stable order.
    key: String,
}

impl RankedSite {
    fn new(mut members: Vec<RawEscalation>) -> Self {
        members.sort_by(|a, b| {
            (a.reason.rank(), rank_sort_key(a.stage2_rank), &a.key).cmp(&(
                b.reason.rank(),
                rank_sort_key(b.stage2_rank),
                &b.key,
            ))
        });
        let severity = members
            .iter()
            .map(|m| m.severity.as_str())
            .max_by_key(|s| crate::ai_audit::severity_rank(s))
            .unwrap_or_default()
            .to_string();
        let stage2_rank = members
            .iter()
            .map(|m| m.stage2_rank)
            .min_by_key(|r| rank_sort_key(*r))
            .unwrap_or(0);
        Self {
            severity,
            reason_rank: members[0].reason.rank(),
            stage2_rank,
            node_id: members[0].node_id.clone(),
            key: members[0].key.clone(),
            members,
        }
    }
}

/// Rank every stage 2 escalation of `nodes` into one list and mark the line (design 4.4).
///
/// 1. Every escalation of every node is collected. One row has one reason: the highest-ranked
///    reason it was given. An escalation naming a site that is not in `rows` is ignored.
/// 2. The engine adds a candidate, reason `evidence_unverified`, for every row whose quoted
///    evidence was not found (D5: the release gate refuses that row without a decision anyway),
///    and lifts a `needs_more_code` escalation of such a row to it.
/// 3. Rows at one site (the same repo, path and line) share one slot whatever their rules (R-1):
///    the site takes its highest severity and its best reason, and every row keeps its own reason.
/// 4. The sites are sorted by severity (critical first), then by reason rank (R-2: a low row with
///    a top reason never outranks a high row), then interleaved across nodes inside one severity
///    and reason (rank 1 of every node before rank 2 of any), so one noisy rule cannot take the
///    whole budget (D6). The same candidates always give the same list.
/// 5. The first `budget` slots are escalated; past it only the D5 reasons at their severity and
///    `evidence_unverified` are, each recording why. The rest stay, in order, below the line.
pub fn rank_candidates(nodes: &[TreeNode], rows: &[Finding], settings: &RankSettings) -> Ranked {
    use std::collections::{BTreeMap, HashMap};

    let site_of = |f: &Finding| (f.repo.clone(), f.path.clone(), f.line);
    let by_key: HashMap<&str, &Finding> = rows.iter().map(|f| (f.site_key.as_str(), f)).collect();
    let mut raw: BTreeMap<String, RawEscalation> = BTreeMap::new();
    for node in nodes {
        for e in &node.escalations {
            let Some(row) = by_key.get(e.key.as_str()) else {
                continue;
            };
            let better = raw.get(&e.key).is_none_or(|have| {
                (e.reason.rank(), rank_sort_key(e.rank))
                    < (have.reason.rank(), rank_sort_key(have.stage2_rank))
            });
            if better {
                raw.insert(
                    e.key.clone(),
                    RawEscalation {
                        node_id: node.id.clone(),
                        key: e.key.clone(),
                        reason: e.reason,
                        stage2_rank: e.rank,
                        severity: row.severity.clone(),
                        why: e.why.clone(),
                        site: site_of(row),
                    },
                );
            }
        }
    }
    for f in rows {
        if !f
            .captures
            .contains_key(crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE)
        {
            continue;
        }
        match raw.get_mut(&f.site_key) {
            Some(have) if have.reason.rank() > EscalationReason::EvidenceUnverified.rank() => {
                have.reason = EscalationReason::EvidenceUnverified;
            }
            Some(_) => {}
            None => {
                raw.insert(
                    f.site_key.clone(),
                    RawEscalation {
                        node_id: f.group_path.get(1).cloned().unwrap_or_default(),
                        key: f.site_key.clone(),
                        reason: EscalationReason::EvidenceUnverified,
                        stage2_rank: 0,
                        severity: f.severity.clone(),
                        why: "The quoted evidence was not found in the scanned file, so the \
                              release gate needs a decision on this site."
                            .to_string(),
                        site: site_of(f),
                    },
                );
            }
        }
    }

    // One slot per site.
    let mut by_site: BTreeMap<(String, String, usize), Vec<RawEscalation>> = BTreeMap::new();
    for r in raw.into_values() {
        by_site.entry(r.site.clone()).or_default().push(r);
    }
    let sites: Vec<RankedSite> = by_site.into_values().map(RankedSite::new).collect();

    // Severity, then reason rank (R-2), then node; each node's list sorted by its own rank.
    let sev = |s: &str| std::cmp::Reverse(crate::ai_audit::severity_rank(s));
    let mut by_reason: BTreeMap<_, BTreeMap<String, Vec<RankedSite>>> = BTreeMap::new();
    for site in sites {
        by_reason
            .entry((sev(&site.severity), site.reason_rank))
            .or_default()
            .entry(site.node_id.clone())
            .or_default()
            .push(site);
    }
    let mut ordered: Vec<RankedSite> = Vec::new();
    for per_node in by_reason.into_values() {
        let mut lists: Vec<(String, std::collections::VecDeque<RankedSite>)> = per_node
            .into_iter()
            .map(|(node, mut items)| {
                items.sort_by(|a, b| {
                    (rank_sort_key(a.stage2_rank), &a.key)
                        .cmp(&(rank_sort_key(b.stage2_rank), &b.key))
                });
                (node, items.into())
            })
            .collect();
        loop {
            let mut round: Vec<RankedSite> = lists
                .iter_mut()
                .filter_map(|(_, items)| items.pop_front())
                .collect();
            if round.is_empty() {
                break;
            }
            round.sort_by(|a, b| {
                (rank_sort_key(a.stage2_rank), &a.node_id, &a.key).cmp(&(
                    rank_sort_key(b.stage2_rank),
                    &b.node_id,
                    &b.key,
                ))
            });
            ordered.extend(round);
        }
    }

    let mut candidates: Vec<EscalationCandidate> = Vec::new();
    for (slot, site) in ordered.into_iter().enumerate() {
        for r in site.members {
            candidates.push(EscalationCandidate {
                key: r.key,
                node_id: r.node_id,
                reason: r.reason,
                severity: site.severity.clone(),
                stage2_rank: r.stage2_rank,
                global_rank: candidates.len() as u32 + 1,
                slot: slot as u32 + 1,
                escalated: false,
                over_budget_reason: None,
                why: r.why,
            });
        }
    }
    let mut budget = EscalationBudget::compute(
        settings.rate_per_kloc,
        settings.floor,
        settings.counted_lines,
    );
    camerata_api_types::review::re_mark(
        &mut candidates,
        &mut budget,
        &settings.over_budget_reasons,
    );
    Ranked { candidates, budget }
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
mod outlier_tests;
#[cfg(test)]
mod rank_tests;
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
