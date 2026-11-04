//! The tree and its sites (review UI spec section 5; U1).
//!
//! [`ReviewComposer::tree`] returns nodes, counts and markers and never a site: its size follows
//! the number of nodes. [`ReviewComposer::sites_page`] returns one node's sites, 500 at a time, in
//! a fixed order with a cursor that names the last row returned.

use std::collections::HashMap;

use camerata_api_types::decisions::Scope;
use camerata_api_types::edits::EditField;
use camerata_api_types::review_view::{
    EditView, NodeCountsView, NodeDecisionSummary, SitesPage, SummarySource, TreeLevel,
    TreeNodeKind, TreeNodeView, TreeView, SITES_PAGE_SIZE,
};

use super::ReviewComposer;
use crate::onboard::Finding;
use crate::report_export::tree_view::prose_flags_of;
use crate::review_tree::{is_thrown_out_id, NodeKind, NodeLevel, TextSource, TreeNode};

/// The rank of a severity for ordering: critical first. An unknown severity sorts last.
pub fn severity_rank(severity: &str) -> u8 {
    match severity.to_ascii_lowercase().as_str() {
        "critical" => 0,
        "high" => 1,
        "medium" => 2,
        "low" => 3,
        "info" => 4,
        _ => 5,
    }
}

/// The unit separator, between the parts of a cursor.
const SEP: char = '\u{1f}';

/// The cursor naming a row: its severity rank, path, line and site key. Hex, so it travels in a
/// query string untouched.
pub fn encode_cursor(f: &Finding) -> String {
    let raw = format!(
        "{}{SEP}{}{SEP}{}{SEP}{}",
        severity_rank(&f.severity),
        f.path,
        f.line,
        f.site_key
    );
    raw.bytes().map(|b| format!("{b:02x}")).collect()
}

/// The (rank, path, line, key) a cursor names, or `None` for a cursor that is not one.
pub fn decode_cursor(cursor: &str) -> Option<(u8, String, usize, String)> {
    if cursor.is_empty() || !cursor.len().is_multiple_of(2) || !cursor.is_ascii() {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..cursor.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&cursor[i..i + 2], 16).ok())
        .collect();
    let raw = String::from_utf8(bytes?).ok()?;
    let mut parts = raw.splitn(4, SEP);
    let rank = parts.next()?.parse().ok()?;
    let path = parts.next()?.to_string();
    let line = parts.next()?.parse().ok()?;
    let key = parts.next()?.to_string();
    Some((rank, path, line, key))
}

/// The order sites are listed in.
fn site_order(f: &Finding) -> (u8, &str, usize, &str) {
    (
        severity_rank(&f.severity),
        f.path.as_str(),
        f.line,
        f.site_key.as_str(),
    )
}

/// Counts for one node as they accumulate.
#[derive(Default, Clone, Copy)]
struct Tally(NodeCountsView);

impl<'a> ReviewComposer<'a> {
    /// The tree's node at position `i`.
    pub(super) fn node_ref(&self, i: usize) -> &'a TreeNode {
        &self
            .tree
            .expect("a node index exists only with a tree")
            .nodes[i]
    }

    /// The site keys under `node_id`: its own members and those of every node below it, thrown-out
    /// nodes only when `node_id` is one itself (a decision on a category or a rule never reaches a
    /// row the engine threw out).
    pub(crate) fn members_under(&self, node_id: &str) -> Option<Vec<&'a str>> {
        let start = *self.node_at.get(node_id)?;
        let from_thrown_out = is_thrown_out_id(node_id);
        let mut out: Vec<&str> = Vec::new();
        let mut stack = vec![start];
        while let Some(i) = stack.pop() {
            let node = self.node_ref(i);
            out.extend(node.members.iter().map(String::as_str));
            if let Some(kids) = self.children.get(node.id.as_str()) {
                stack.extend(
                    kids.iter()
                        .copied()
                        .filter(|k| from_thrown_out || !is_thrown_out_id(&self.node_ref(*k).id)),
                );
            }
        }
        out.sort_unstable();
        out.dedup();
        Some(out)
    }

    /// The counts every node shows, worked out in one pass over the nodes that hold sites and
    /// then added up the tree.
    fn node_counts(&self) -> HashMap<&'a str, Tally> {
        let Some(tree) = self.tree else {
            return HashMap::new();
        };
        let mut own: HashMap<&str, Tally> = HashMap::new();
        for node in tree.nodes.iter().filter(|n| !n.members.is_empty()) {
            let mut t = NodeCountsView {
                sites: node.members.len(),
                ..NodeCountsView::default()
            };
            for key in &node.members {
                let standing = self.standings.get(key);
                if let Some(c) = self.candidate(key) {
                    if c.escalated {
                        t.escalated += 1;
                    } else {
                        t.below_the_line += 1;
                    }
                }
                let marked_outlier = self.outliers.contains(key.as_str())
                    || self
                        .row(key)
                        .is_some_and(|f| f.review.outlier_reason.is_some());
                t.outliers += usize::from(marked_outlier);
                t.new_since_decision +=
                    usize::from(standing.is_some_and(|s| s.new_since_decision.is_some()));
                let stale_edit = self.edits_by_key.get(key.as_str()).is_some_and(|edits| {
                    edits
                        .iter()
                        .any(|e| self.stale_edits.contains_key(e.id.as_str()))
                });
                t.stale += usize::from(standing.is_some_and(|s| s.stale.is_some()) || stale_edit);
                t.held += usize::from(self.hold_of(key).is_some());
                t.decided += usize::from(standing.is_some_and(|s| s.decision.is_some()));
            }
            if node.id.ends_with("/unplaced") {
                t.unplaced = node.members.len();
            }
            own.insert(node.id.as_str(), Tally(t));
        }

        let mut out: HashMap<&str, Tally> = HashMap::new();
        for (id, tally) in own {
            let thrown = is_thrown_out_id(id);
            let mut at = Some(id);
            while let Some(node_id) = at {
                // A thrown-out node's sites never count toward a node that is not thrown out.
                if thrown && !is_thrown_out_id(node_id) {
                    break;
                }
                let sum = out.entry(node_id).or_default();
                sum.0.sites += tally.0.sites;
                sum.0.escalated += tally.0.escalated;
                sum.0.below_the_line += tally.0.below_the_line;
                sum.0.outliers += tally.0.outliers;
                sum.0.new_since_decision += tally.0.new_since_decision;
                sum.0.stale += tally.0.stale;
                sum.0.held += tally.0.held;
                sum.0.decided += tally.0.decided;
                sum.0.unplaced += tally.0.unplaced;
                at = self
                    .node_at
                    .get(node_id)
                    .and_then(|i| self.node_ref(*i).parent.as_deref());
            }
        }
        out
    }

    /// `GET /api/projects/:id/review/tree`: nodes, counts and markers; never a site.
    pub fn tree(&self) -> TreeView {
        let Some(tree) = self.tree else {
            return TreeView {
                run_id: self.inputs.run_id.to_string(),
                group_reviewed: false,
                nodes: Vec::new(),
            };
        };
        let counts = self.node_counts();
        let nodes = tree
            .nodes
            .iter()
            .map(|n| TreeNodeView {
                id: n.id.clone(),
                level: match n.level {
                    NodeLevel::Category => TreeLevel::Category,
                    NodeLevel::Rule => TreeLevel::Rule,
                    NodeLevel::Subgroup => TreeLevel::Subgroup,
                },
                parent: n.parent.clone(),
                label: n.label.clone(),
                kind: match n.kind {
                    NodeKind::Escalation => TreeNodeKind::Escalation,
                    NodeKind::Bulk => TreeNodeKind::Bulk,
                },
                cause: n.cause.clone(),
                summary: n.summary.clone(),
                summary_source: match n.summary_source {
                    TextSource::Model => SummarySource::Model,
                    TextSource::Reviewer => SummarySource::Reviewer,
                },
                flags: n.flags.iter().cloned().chain(prose_flags_of(n)).collect(),
                thrown_out: is_thrown_out_id(&n.id),
                counts: counts
                    .get(n.id.as_str())
                    .map_or_else(NodeCountsView::default, |t| t.0),
                decision: self.node_decision(&n.id),
                summary_edit: self.summary_edit_of(n),
            })
            .collect();
        TreeView {
            run_id: self.inputs.run_id.to_string(),
            group_reviewed: tree.group_reviewed,
            nodes,
        }
    }

    /// The reviewer's latest summary edit of `node` (spec U4), applied or stale, beside the node.
    fn summary_edit_of(&self, node: &TreeNode) -> Option<EditView> {
        let edit = self
            .edits_by_key
            .get(node.id.as_str())?
            .iter()
            .rfind(|e| e.field == EditField::Summary)?;
        let stale = self.stale_edits.get(edit.id.as_str());
        Some(EditView {
            id: edit.id.clone(),
            field: edit.field,
            old: edit.old.clone(),
            new: edit.new.clone(),
            reason: edit.reason.clone(),
            by: edit.by.clone(),
            at: edit.at.clone(),
            stale: stale.is_some(),
            current_value: stale.map_or_else(
                || node.summary.clone().unwrap_or_default(),
                |s| s.current_value.clone(),
            ),
            reapplied_from: edit.reapplied_from.clone(),
        })
    }

    /// The live group decision standing on `node_id`, the latest if there are several.
    fn node_decision(&self, node_id: &str) -> Option<NodeDecisionSummary> {
        self.inputs
            .reviewed
            .records()
            .iter()
            .rfind(|d| matches!(&d.scope, Scope::Node(n) if n == node_id))
            .map(|d| NodeDecisionSummary {
                id: d.id.clone(),
                state: d.state.clone(),
                reason: d.reason.clone(),
                decided_at: d.decided_at.clone(),
                covered: d.applies_to.len(),
                left_out: d.except.len(),
            })
    }

    /// `GET /api/projects/:id/review/sites?node=&cursor=&limit=`. `None` when the tree has no such
    /// node. `limit` is clamped to `1..=500`; `cursor` is the previous page's `next_cursor`, and a
    /// cursor that is not one starts from the top.
    pub fn sites_page(
        &self,
        node_id: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Option<SitesPage> {
        let keys = self.members_under(node_id)?;
        let mut rows: Vec<&Finding> = keys.into_iter().filter_map(|k| self.row(k)).collect();
        rows.sort_by(|a, b| site_order(a).cmp(&site_order(b)));
        let total = rows.len();
        let limit = limit.clamp(1, SITES_PAGE_SIZE);
        // The cursor names the last row of the previous page: the next page starts after where
        // that row sorts, whether or not the row is still there.
        let start = cursor
            .and_then(decode_cursor)
            .map_or(0, |(rank, path, line, key)| {
                rows.partition_point(|f| site_order(f) <= (rank, path.as_str(), line, key.as_str()))
            });
        let page = &rows[start..(start + limit).min(total)];
        let next_cursor = (start + page.len() < total)
            .then(|| page.last().map(|f| encode_cursor(f)))
            .flatten();
        Some(SitesPage {
            run_id: self.inputs.run_id.to_string(),
            node: node_id.to_string(),
            total,
            offset: start,
            rows: page.iter().map(|f| self.site_row(f)).collect(),
            next_cursor,
        })
    }
}
