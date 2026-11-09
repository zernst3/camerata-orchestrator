//! The tree types (design section 3.1) and the small records a row carries about its place in it.
//!
//! The tree is category, then rule, then sub-group, then sites. Every row of a report is in
//! exactly one sub-group; nothing in this module removes, hides or merges a row. A [`TreeNode`]
//! only names rows by their site keys.

use serde::{Deserialize, Serialize};

/// One source that was collapsed into a surviving row because it was mechanically identical to
/// it (same repo and file, same rule family, same place). The survivor keeps one entry per
/// collapsed source, so the client still sees every rule and tool that flagged the place, and a
/// decision on the survivor covers the source by key. The source keeps its OWN path and line
/// (amendment A1): a collapse never loses a location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlaggedBy {
    /// The rule id the source was flagged under.
    pub rule_id: String,
    /// The external tool that produced the source, when it came from one.
    #[serde(default)]
    pub tool: Option<String>,
    /// The source's own site key (what it would have been named had it stayed a row).
    pub site_key: String,
    /// The file the source cited.
    pub path: String,
    /// The line the source cited (1-based; 0 when it cited none).
    pub line: usize,
}

/// A possible duplicate: another row that may describe the same thing. Only ever a pointer. A
/// link never changes a row count or a severity, and a batch decision never follows one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateLink {
    /// The site key of the higher-ranked row this one may duplicate.
    pub key: String,
    /// The test that fired (`same_line`, `same_cause:<cause>`, `same_object_same_file`).
    pub hint: String,
}

/// A row's standing inside its sub-group. The primary orders the sub-group (it is the row the
/// engine would show first) and has no power over the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GroupRole {
    #[default]
    Primary,
    Member,
}

impl GroupRole {
    /// Serde helper: a primary is the default and is left off the wire.
    pub fn is_primary(&self) -> bool {
        matches!(self, Self::Primary)
    }
}

/// Which level of the tree a node is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeLevel {
    Category,
    Rule,
    Subgroup,
}

/// Whether a node is read site by site or decided as a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NodeKind {
    /// A node with a security-class or high-severity site: the reviewer expects to open it.
    Escalation,
    /// A node of routine sites that can be decided as a batch.
    Bulk,
}

/// Who put a site on a node's outlier list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OutlierBy {
    Engine,
    Model,
}

pub use camerata_api_types::review::OutlierReason;

/// One outlier on a node: a site (by key), why, and who said so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outlier {
    pub key: String,
    pub reason: OutlierReason,
    pub by: OutlierBy,
    /// The group reviewer's one-line reason for marking the site; empty for an engine mark.
    #[serde(default)]
    pub why: String,
}

/// One site the group reviewer wants the architect to look at, as it said it on its own node.
/// The ranking ([`super::rank_candidates`]) reads these from every node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeEscalation {
    /// The site key.
    pub key: String,
    pub reason: camerata_api_types::review::EscalationReason,
    /// The group reviewer's rank inside the node (1 is first).
    pub rank: u32,
    /// Its one-line reason, in the architect's words.
    pub why: String,
}

/// Where a node's summary text came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TextSource {
    #[default]
    Model,
    Reviewer,
}

/// Any level above a site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeNode {
    /// `cat:authorization`, `rule:SEC-RLS-1`, `sub:SEC-RLS-1/3`.
    pub id: String,
    pub level: NodeLevel,
    pub parent: Option<String>,
    pub label: String,
    /// Every rule id folded into the node (native, external tool, AI-), sorted.
    pub rule_ids: Vec<String>,
    pub kind: NodeKind,
    /// Why the sites of a sub-group belong together. Sub-group only.
    pub cause: Option<String>,
    /// Site keys of the direct members. Only a sub-group has direct members.
    pub members: Vec<String>,
    pub outliers: Vec<Outlier>,
    /// What the reviewer reads at this node (written by the group reviewer).
    pub summary: Option<String>,
    #[serde(default)]
    pub summary_source: TextSource,
    /// The sites the group reviewer wants escalated, from this node's stage 2 call. Rule nodes
    /// only.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub escalations: Vec<NodeEscalation>,
    /// Review flags on the node's summary: a count it states that the data contradicts, or a
    /// differing member it does not name (A9). Never shown to the client.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub flags: Vec<String>,
}

/// The counts shown beside a node. Every one is read from the tree and the rows, never from the
/// group reviewer's prose (A9).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeCounts {
    /// Sites under the node (thrown-out sites only when the node is a thrown-out node).
    pub sites: usize,
    /// Sites under the node that are escalated.
    pub escalated: usize,
    /// Sites under the node that are candidates below the line.
    pub below_the_line: usize,
    /// Sites under the node with an outlier mark (by the engine or the group reviewer).
    pub outliers: usize,
    /// Sites under the node the group reviewer did not place in a sub-group.
    pub unplaced: usize,
}

/// The review tree of one report. Sealed with it, stored in the inspection entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReviewTree {
    pub run_id: String,
    pub engine_version: String,
    /// Every level, parents before children.
    pub nodes: Vec<TreeNode>,
    /// How many escalations the run may carry and what came of it (4.4).
    #[serde(default)]
    pub budget: camerata_api_types::review::EscalationBudget,
    /// Every site the group reviewer wanted to escalate, in global rank order, above and below
    /// the line. Moving the line re-marks these with no model call.
    #[serde(default)]
    pub candidates: Vec<camerata_api_types::review::EscalationCandidate>,
    /// False until the group reviewer has read the tree. An old inspection unfolded on load has a
    /// tree that no group reviewer has seen.
    #[serde(default)]
    pub group_reviewed: bool,
    /// The Fixes: the sub-groups whose places are one change ([`super::merge_fixes`]). Worked out
    /// from the rows and the tree alone, never from a decision, and stored with the tree. A tree
    /// stored before Fixes existed has none; an export works them out from the rows then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fixes: Vec<super::FixNode>,
    /// The places with two or more problem rows ([`super::merge_places`]): the row that heads
    /// each and the rules it carries. Worked out from the rows and the tree alone, never from a
    /// decision, and stored with the tree. A tree stored before places existed has none; an
    /// export works them out from the rows then.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub places: Vec<super::PlaceNode>,
}

impl ReviewTree {
    /// Write the stored candidates onto the rows' columns (the escalation state, rank and
    /// reasons), and clear the columns of rows that are not candidates.
    pub fn sync_rows(&self, rows: &mut [crate::onboard::Finding]) {
        let by_key: std::collections::HashMap<
            &str,
            &camerata_api_types::review::EscalationCandidate,
        > = self
            .candidates
            .iter()
            .map(|c| (c.key.as_str(), c))
            .collect();
        for f in rows {
            match by_key.get(f.site_key.as_str()) {
                Some(c) => f.review.set_escalation(c),
                None => f.review.clear_escalation(),
            }
        }
    }

    /// Move the line again over the stored candidates with new settings: no model call, no
    /// candidate added or dropped. Rewrites the budget, the candidates' marks and the rows'
    /// columns.
    pub fn re_mark(
        &mut self,
        settings: &super::RankSettings,
        rows: &mut [crate::onboard::Finding],
    ) {
        self.budget = camerata_api_types::review::EscalationBudget::compute(
            settings.rate_per_kloc,
            settings.floor,
            settings.counted_lines,
        );
        camerata_api_types::review::re_mark(
            &mut self.candidates,
            &mut self.budget,
            &settings.over_budget_reasons,
        );
        self.sync_rows(rows);
    }

    /// The counts of `node_id`, from the tree alone: its direct members and those of every node
    /// below it, the stored candidates and the outlier lists.
    pub fn counts(&self, node_id: &str) -> NodeCounts {
        let members: std::collections::BTreeSet<String> =
            self.batch_members(node_id).into_iter().collect();
        // The node and every node below it, thrown-out nodes only when the node is one itself.
        let from_thrown_out = super::build::is_thrown_out_id(node_id);
        let mut below: Vec<&TreeNode> = Vec::new();
        let mut stack: Vec<&str> = vec![node_id];
        while let Some(id) = stack.pop() {
            if let Some(n) = self.node(id) {
                below.push(n);
            }
            stack.extend(
                self.nodes
                    .iter()
                    .filter(|n| n.parent.as_deref() == Some(id))
                    .filter(|n| from_thrown_out || !super::build::is_thrown_out_id(&n.id))
                    .map(|n| n.id.as_str()),
            );
        }
        let outliers: std::collections::BTreeSet<&str> = below
            .iter()
            .flat_map(|n| n.outliers.iter())
            .map(|o| o.key.as_str())
            .filter(|k| members.contains(*k))
            .collect();
        let unplaced = below
            .iter()
            .filter(|n| n.id.ends_with("/unplaced"))
            .flat_map(|n| n.members.iter())
            .count();
        let mut escalated = 0;
        let mut below_the_line = 0;
        for c in self.candidates.iter().filter(|c| members.contains(&c.key)) {
            if c.escalated {
                escalated += 1;
            } else {
                below_the_line += 1;
            }
        }
        NodeCounts {
            sites: members.len(),
            escalated,
            below_the_line,
            outliers: outliers.len(),
            unplaced,
        }
    }

    /// The node with this id.
    pub fn node(&self, id: &str) -> Option<&TreeNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The site keys a batch decision on `node_id` covers: the direct members of the node and of
    /// every node below it, sorted, each once. A possible-duplicate link is never followed, so a
    /// batch decision on a node can never reach a row outside the node.
    pub fn batch_members(&self, node_id: &str) -> Vec<String> {
        // A Fix covers the members of every sub-group in it.
        if node_id.starts_with(super::FIX_PREFIX) {
            let mut keys: Vec<String> = self
                .fix(node_id)
                .into_iter()
                .flat_map(|fx| fx.members.iter())
                .flat_map(|sub| self.batch_members(sub))
                .collect();
            keys.sort();
            keys.dedup();
            return keys;
        }
        // A place covers the rows at it.
        if node_id.starts_with(super::PLACE_PREFIX) {
            let mut keys: Vec<String> = self
                .place(node_id)
                .map(|p| p.members.clone())
                .unwrap_or_default();
            keys.sort();
            keys.dedup();
            return keys;
        }
        let mut keys: Vec<String> = Vec::new();
        let mut stack: Vec<&str> = vec![node_id];
        while let Some(id) = stack.pop() {
            let Some(node) = self.node(id) else {
                continue;
            };
            keys.extend(node.members.iter().cloned());
            // A thrown-out node is reached only by naming it: a decision on a category or a rule
            // never writes to a row the engine threw out (restoring one is its own decision).
            let from_thrown_out = super::build::is_thrown_out_id(node_id);
            stack.extend(
                self.nodes
                    .iter()
                    .filter(|n| n.parent.as_deref() == Some(id))
                    .filter(|n| from_thrown_out || !super::build::is_thrown_out_id(&n.id))
                    .map(|n| n.id.as_str()),
            );
        }
        keys.sort();
        keys.dedup();
        keys
    }
}
