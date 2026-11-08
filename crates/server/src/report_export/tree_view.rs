//! The review tree as a client package prints it (design 3B): the category, rule and sub-group
//! nodes with their label, cause and summary, and the counts beside each, then one node for each
//! Fix, the sub-groups whose places are one change (item #1).
//!
//! The counts are read from the rows (A9 item 1), never from the group reviewer's prose, and every
//! text the group reviewer wrote passes M4's prose checks first ([`super::prose_checks`]). A node
//! the engine threw out has no place here: the client export never reads `discarded`. An internal
//! package adds what the architect triages by: where each summary came from, the escalated,
//! below-the-line and outlier counts, and every review flag.

use std::collections::BTreeMap;

use serde::Serialize;

use super::prose_checks::client_text;
use super::ReviewedReport;
use crate::review_tree::{is_thrown_out_id, NodeLevel, TextSource, TreeNode};

/// One node as a client package prints it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct TreeNodeView {
    pub id: String,
    /// `category`, `rule`, `subgroup` or `fix`.
    pub level: String,
    pub parent: String,
    /// The Fix a rule group is in, or a Fix's own id. Empty above the rule groups.
    pub fix_id: String,
    pub label: String,
    /// Why the sites of a sub-group belong together; empty above the sub-group.
    pub cause: String,
    /// What the reviewer reads at the node; empty when none was written or the text was withheld.
    pub summary: String,
    /// Every site under the node: kept rows, false positives and dependency advisories.
    pub sites: usize,
    /// How many of those the reviewer marked a false positive.
    pub false_positives: usize,
    /// Sites under the node by review level wire name.
    pub review_levels: BTreeMap<String, usize>,
    /// The architect's triage of the node. Internal packages only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub internal: Option<NodeTriage>,
}

/// What an internal package adds to a node.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct NodeTriage {
    /// `model` or `reviewer`.
    pub summary_source: String,
    pub escalated: usize,
    pub below_the_line: usize,
    pub outliers: usize,
    pub unplaced: usize,
    /// Review flags on the node: the group reviewer's summary checks (A9) and the prose checks
    /// that withheld a text from client output.
    pub flags: Vec<String>,
}

fn level_name(level: NodeLevel) -> &'static str {
    match level {
        NodeLevel::Category => "category",
        NodeLevel::Rule => "rule",
        NodeLevel::Subgroup => "subgroup",
    }
}

/// The tree of `reviewed` for a package, parents before children, thrown-out nodes left out.
pub fn tree_view(reviewed: &ReviewedReport, internal: bool) -> Vec<TreeNodeView> {
    let report = reviewed.report();
    let Some(tree) = report.tree.as_ref() else {
        return Vec::new();
    };
    // Sites, false positives and review levels per node id, counted from the rows.
    let mut sites: BTreeMap<&str, usize> = BTreeMap::new();
    let mut fps: BTreeMap<&str, usize> = BTreeMap::new();
    let mut levels: BTreeMap<&str, BTreeMap<String, usize>> = BTreeMap::new();
    for f in &report.findings {
        let fp = reviewed
            .dispositions()
            .get(&super::finding_key(f))
            .is_some_and(|d| d.state == "FalsePositive");
        let level = reviewed
            .review_level(f)
            .map(camerata_api_types::review::ReviewLevel::as_wire);
        for id in f.group_path.iter().take(3) {
            *sites.entry(id.as_str()).or_default() += 1;
            if fp {
                *fps.entry(id.as_str()).or_default() += 1;
            }
            if let Some(level) = &level {
                *levels
                    .entry(id.as_str())
                    .or_default()
                    .entry(level.clone())
                    .or_default() += 1;
            }
        }
    }
    let fixes = crate::review_tree::FixIndex::new(Some(tree), &report.findings);
    let mut out: Vec<TreeNodeView> = tree
        .nodes
        .iter()
        .filter(|n| !is_thrown_out_id(&n.id))
        .map(|n| {
            let (view_flags, label, cause, summary) = checked_text(n);
            let counts = tree.counts(&n.id);
            TreeNodeView {
                id: n.id.clone(),
                level: level_name(n.level).to_string(),
                parent: n.parent.clone().unwrap_or_default(),
                fix_id: if n.level == NodeLevel::Subgroup {
                    fixes.fix_of_subgroup(&n.id).unwrap_or_default().to_string()
                } else {
                    String::new()
                },
                label,
                cause,
                summary,
                sites: sites.get(n.id.as_str()).copied().unwrap_or(0),
                false_positives: fps.get(n.id.as_str()).copied().unwrap_or(0),
                review_levels: levels.get(n.id.as_str()).cloned().unwrap_or_default(),
                internal: internal.then(|| NodeTriage {
                    summary_source: match n.summary_source {
                        TextSource::Model => "model",
                        TextSource::Reviewer => "reviewer",
                    }
                    .to_string(),
                    escalated: counts.escalated,
                    below_the_line: counts.below_the_line,
                    outliers: counts.outliers,
                    unplaced: counts.unplaced,
                    flags: n.flags.iter().cloned().chain(view_flags).collect(),
                }),
            }
        })
        .collect();
    // One node for each Fix, after the tree: it is named by its anchor rule group and counts every
    // row of the rule groups folded into it.
    for fx in fixes.fixes() {
        let members: Vec<&TreeNodeView> = out
            .iter()
            .filter(|n| n.level == "subgroup" && fx.members.contains(&n.id))
            .collect();
        let Some(anchor) = members.first() else {
            continue;
        };
        let mut review_levels: BTreeMap<String, usize> = BTreeMap::new();
        for m in &members {
            for (level, n) in &m.review_levels {
                *review_levels.entry(level.clone()).or_default() += n;
            }
        }
        let node = TreeNodeView {
            id: fx.id.clone(),
            level: "fix".to_string(),
            parent: String::new(),
            fix_id: fx.id.clone(),
            label: anchor.label.clone(),
            cause: anchor.cause.clone(),
            summary: anchor.summary.clone(),
            sites: members.iter().map(|m| m.sites).sum(),
            false_positives: members.iter().map(|m| m.false_positives).sum(),
            review_levels,
            internal: None,
        };
        out.push(node);
    }
    out
}

/// The flags the prose checks give a node's label, cause and summary, one per hit (`summary:
/// methodology_leak:the scanner`). The review screen shows them beside the node before any client
/// package is built, so they are worked out here once and read by the screen and the export alike.
pub(crate) fn prose_flags_of(n: &TreeNode) -> Vec<String> {
    checked_text(n).0
}

/// A node's flags from the prose checks, and its label, cause and summary as client output may
/// carry them. A withheld label falls back to the plain name of the level.
fn checked_text(n: &TreeNode) -> (Vec<String>, String, String, String) {
    let label = client_text("label", &n.label);
    let cause = client_text("cause", n.cause.as_deref().unwrap_or_default());
    let summary = client_text("summary", n.summary.as_deref().unwrap_or_default());
    let flags: Vec<String> = label
        .flags
        .iter()
        .chain(&cause.flags)
        .chain(&summary.flags)
        .cloned()
        .collect();
    let label_text = if label.flags.is_empty() {
        label.text
    } else {
        format!("{} group", level_name(n.level))
    };
    (flags, label_text, cause.text, summary.text)
}
