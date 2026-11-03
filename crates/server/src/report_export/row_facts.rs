//! What every exported row says about its place in the review (design 3B items 2 and 3, amendment
//! A5), worked out once from the [`ReviewedReport`] and written by every exporter.
//!
//! - [`RowFacts`] is on every row of every export: the findings sheets, `findings.json`, the
//!   `csv/` and `json/` datasets, the Sites sheet and the dependency rows. It names the row
//!   ([`site_id`]), how closely it was reviewed, the sub-group it sits in and that sub-group's
//!   label and cause, and whether the group reviewer escalated it.
//! - [`InternalTriage`] is the group reviewer's triage of a row: why it was escalated, its rank,
//!   the outlier mark, the disagreement with stage 1 and the model's own words for each. It is
//!   written ONLY to the INTERNAL workbook's columns. It is in no JSON or CSV file of any package,
//!   and a client package carries none of it (the wave 5 review, and A8).
//!
//! Neither holds reviewer text: a reviewer's reasons and names stay in the stores.

use std::collections::HashMap;

use camerata_api_types::review::EscalationState;
use serde::Serialize;

use super::ReviewedReport;
use crate::onboard::Finding;
use crate::review_tree::TreeNode;

/// A short, printable name for a site key. The key itself holds NUL separators and a repo, path
/// and fingerprint; this is its SHA-256, first 16 hex digits, the same for the same site on every
/// run of the same code. It is what a decisions file, a ticket and a Sites sheet filter use.
pub fn site_id(site_key: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(site_key.as_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What a row says about how it was reviewed and where it sits. Present on every row of every
/// export, so every output filters and counts the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct RowFacts {
    /// [`site_id`] of the row.
    pub site_id: String,
    /// `personally_reviewed`, `reviewed_as_pattern:<node_id>` or `engine_verified`; empty only
    /// for a row held for a ruling, which a client package refuses.
    pub review_level: String,
    /// The sub-group's node id. The Sites sheet filter is `Group = <id>`.
    pub group: String,
    /// The sub-group's label.
    pub sub_group: String,
    /// Why the sub-group's sites belong together.
    pub sub_group_cause: String,
    /// The group reviewer put the site in the architect's queue and it is above the line.
    pub escalated: bool,
    /// The [`site_id`] of the row this one was merged into, when the reviewer merged it.
    pub merged_into: String,
}

/// The group reviewer's triage of one row, as text. The internal workbook only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct InternalTriage {
    /// `escalated`, `below_the_line` or empty.
    pub escalation_state: String,
    pub escalation_reason: String,
    pub stage2_rank: String,
    pub global_rank: String,
    pub over_budget_reason: String,
    pub escalation_why: String,
    pub outlier_reason: String,
    pub outlier_why: String,
    /// "severity: medium, group reviewer: critical (why)" for each disagreement, joined by "; ".
    pub stage2_disagreement: String,
}

/// The labels of [`InternalTriage`]'s columns on an internal workbook, in field order.
pub const INTERNAL_TRIAGE_HEADERS: [&str; 9] = [
    "Escalation",
    "Escalation reason",
    "Stage 2 rank",
    "Global rank",
    "Over-budget reason",
    "Escalation why",
    "Outlier reason",
    "Outlier why",
    "Stage 2 disagreement",
];

impl InternalTriage {
    /// The cells, in [`INTERNAL_TRIAGE_HEADERS`] order.
    pub fn cells(&self) -> [&str; 9] {
        [
            &self.escalation_state,
            &self.escalation_reason,
            &self.stage2_rank,
            &self.global_rank,
            &self.over_budget_reason,
            &self.escalation_why,
            &self.outlier_reason,
            &self.outlier_why,
            &self.stage2_disagreement,
        ]
    }
}

/// Look-ups over a reviewed report, built once per export.
pub struct RowFactsIndex<'a> {
    reviewed: &'a ReviewedReport,
    nodes: HashMap<&'a str, &'a TreeNode>,
    ids: HashMap<&'a str, String>,
}

impl<'a> RowFactsIndex<'a> {
    pub fn new(reviewed: &'a ReviewedReport) -> Self {
        let report = reviewed.report();
        let nodes = report
            .tree
            .as_ref()
            .map(|t| t.nodes.iter().map(|n| (n.id.as_str(), n)).collect())
            .unwrap_or_default();
        let ids = report
            .findings
            .iter()
            .map(|f| (f.site_key.as_str(), site_id(&f.site_key)))
            .collect();
        Self {
            reviewed,
            nodes,
            ids,
        }
    }

    /// The facts of `f`.
    pub fn facts(&self, f: &Finding) -> RowFacts {
        let group = f.group_path.get(2).cloned().unwrap_or_default();
        let node = self.nodes.get(group.as_str());
        let merged_into = super::overlay::merged_target(f)
            .map(|target| {
                self.ids
                    .get(target)
                    .cloned()
                    .unwrap_or_else(|| site_id(target))
            })
            .unwrap_or_default();
        RowFacts {
            site_id: site_id(&f.site_key),
            review_level: self
                .reviewed
                .review_level(f)
                .map(camerata_api_types::review::ReviewLevel::as_wire)
                .unwrap_or_default(),
            sub_group: node.map(|n| n.label.clone()).unwrap_or_default(),
            sub_group_cause: node.and_then(|n| n.cause.clone()).unwrap_or_default(),
            group,
            escalated: f.review.escalation_state == EscalationState::Escalated,
            merged_into,
        }
    }
}

/// The group reviewer's triage of `f` as text.
pub fn internal_triage(f: &Finding) -> InternalTriage {
    let r = &f.review;
    InternalTriage {
        escalation_state: match r.escalation_state {
            EscalationState::Escalated => "escalated",
            EscalationState::BelowTheLine => "below_the_line",
            EscalationState::NotACandidate => "",
        }
        .to_string(),
        escalation_reason: r
            .escalation_reason
            .map(|x| x.as_str().to_string())
            .unwrap_or_default(),
        stage2_rank: r.stage2_rank.map(|n| n.to_string()).unwrap_or_default(),
        global_rank: r.global_rank.map(|n| n.to_string()).unwrap_or_default(),
        over_budget_reason: r
            .over_budget_reason
            .map(|x| x.as_str().to_string())
            .unwrap_or_default(),
        escalation_why: r.escalation_why.clone(),
        outlier_reason: r
            .outlier_reason
            .map(|x| x.as_str().to_string())
            .unwrap_or_default(),
        outlier_why: r.outlier_why.clone(),
        stage2_disagreement: r
            .stage2_disagreement
            .iter()
            .map(|d| {
                format!(
                    "{}: stage 1 {}, group reviewer {} ({})",
                    d.field.as_str(),
                    d.stage1,
                    d.mine,
                    d.why
                )
            })
            .collect::<Vec<_>>()
            .join("; "),
    }
}
