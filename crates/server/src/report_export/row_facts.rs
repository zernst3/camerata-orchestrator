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

use super::prose_checks::client_text;
use super::ReviewedReport;
use crate::onboard::Finding;

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
    /// The sub-group's node id.
    pub group: String,
    /// The Fix the sub-group is in (`review_tree::merge_fixes`): the same on every row of the
    /// Fix, whatever rule it is under. The All problems filter is `Fix id = <id>`. Empty for a
    /// row in no sub-group.
    pub fix_id: String,
    /// The sub-group's label.
    pub sub_group: String,
    /// Why the sub-group's sites belong together.
    pub sub_group_cause: String,
    /// The group reviewer put the site in the architect's queue and it is above the line.
    pub escalated: bool,
    /// The [`site_id`] of the row this one was merged into, when the reviewer merged it.
    pub merged_into: String,
    /// The [`site_id`] of the row that carries this one: another row at the same place
    /// (repo, file and line) that heads it. Empty for a row that is alone at its place and for
    /// the carrier itself. The carrier lists this row's rule in its `also_matches`.
    pub carried_by: String,
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
    /// Each node's label and cause as client output may carry them, checked once.
    texts: HashMap<&'a str, (String, String)>,
    ids: HashMap<&'a str, String>,
    fixes: crate::review_tree::FixIndex,
    /// Site key of a carried row to the [`site_id`] of the row that carries it.
    carried_by: HashMap<String, String>,
    /// Site key of a carrier to the rule ids of the rows it carries, in the place's order.
    carries: HashMap<String, Vec<String>>,
}

impl<'a> RowFactsIndex<'a> {
    pub fn new(reviewed: &'a ReviewedReport) -> Self {
        let report = reviewed.report();
        // The label and the cause are model prose: every row carries them, so they pass the same
        // checks the plan, the tree, the tickets and the PDF apply (a withheld label reads
        // "Sub-group", a withheld cause is empty).
        let texts = report
            .tree
            .iter()
            .flat_map(|t| &t.nodes)
            .map(|n| {
                let label = client_text("label", &n.label);
                let cause = client_text("cause", n.cause.as_deref().unwrap_or_default());
                let label = if label.flags.is_empty() {
                    label.text
                } else {
                    "Sub-group".to_string()
                };
                (n.id.as_str(), (label, cause.text))
            })
            .collect();
        let ids = report
            .findings
            .iter()
            .map(|f| (f.site_key.as_str(), site_id(&f.site_key)))
            .collect();
        // The places: the rows at one (repo, file, line), the best-ranked row carrying the rest.
        // A false positive is not a problem, so it neither carries nor is carried.
        let dispositions = reviewed.dispositions();
        let is_fp = |f: &Finding| {
            dispositions
                .get(&super::finding_key(f))
                .is_some_and(|d| d.state == "FalsePositive")
        };
        let no_tree = crate::review_tree::ReviewTree::default();
        let places = crate::review_tree::merge_places_where(
            report.tree.as_ref().unwrap_or(&no_tree),
            &report.findings,
            &is_fp,
        );
        let rule_of: HashMap<&str, &str> = report
            .findings
            .iter()
            .map(|f| (f.site_key.as_str(), f.rule_id.as_str()))
            .collect();
        let mut carried_by = HashMap::new();
        let mut carries = HashMap::new();
        for place in &places {
            let carrier_id = site_id(&place.carrier);
            let own_rule = rule_of.get(place.carrier.as_str()).copied().unwrap_or("");
            let mut rules: Vec<String> = Vec::new();
            for key in place.carried() {
                carried_by.insert(key.to_string(), carrier_id.clone());
                let rule = rule_of.get(key).copied().unwrap_or("");
                if !rule.is_empty() && rule != own_rule && !rules.iter().any(|r| r == rule) {
                    rules.push(rule.to_string());
                }
            }
            carries.insert(place.carrier.clone(), rules);
        }
        Self {
            reviewed,
            texts,
            ids,
            fixes: crate::review_tree::FixIndex::new(report.tree.as_ref(), &report.findings),
            carried_by,
            carries,
        }
    }

    /// The rule ids of the rows `f` carries at its place, when it heads one.
    pub fn carried_rules(&self, f: &Finding) -> &[String] {
        self.carries.get(&f.site_key).map_or(&[], Vec::as_slice)
    }

    /// The facts of `f`.
    /// The Fixes the rows' sub-groups are in.
    pub fn fixes(&self) -> &crate::review_tree::FixIndex {
        &self.fixes
    }

    pub fn facts(&self, f: &Finding) -> RowFacts {
        let group = f.group_path.get(2).cloned().unwrap_or_default();
        let (sub_group, sub_group_cause) =
            self.texts.get(group.as_str()).cloned().unwrap_or_default();
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
            sub_group,
            sub_group_cause,
            fix_id: self.fixes.fix_id_of(f),
            group,
            escalated: f.review.escalation_state == EscalationState::Escalated,
            merged_into,
            carried_by: self
                .carried_by
                .get(&f.site_key)
                .cloned()
                .unwrap_or_default(),
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
