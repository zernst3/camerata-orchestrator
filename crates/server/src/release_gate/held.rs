//! Rows held for a ruling of their own that the reviewer's overlay produces (review-tree design
//! 5.3 and A12 item 2).
//!
//! - [`merged_without_decision`] (`CheckId::MergedWithoutDecision`): a merge is a decision with a
//!   reason, so every merged row in the package must have the decision record behind it, and every
//!   group decision must have written the per-site record of each site it covers. A pointer with
//!   no record is a claim nobody made; a group decision missing a member's record would hide a
//!   site nobody looked at behind a ruling that does not name it.
//! - [`restored_without_decision`] (`CheckId::RestoredWithoutDecision`): a finding the engine
//!   threw out and the reviewer put back is held until the reviewer decides it. The restore is
//!   not a ruling.
//!
//! The evidence-unverified check of design 5.3 (`EvidenceUnverifiedWithoutDecision`) is NOT
//! repeated here: [`super::evidence::unverified_without_decision`] (`CheckId::EvidenceUnverified`,
//! M5) already refuses every `evidence_unverified` row that carries no decision, which is exactly
//! that check.
//!
//! The merge check reads the package itself: the merged rows of `findings.json` and the records of
//! `decisions.json`, so what is checked is what the client receives. They are mechanical and hard.

use super::evidence::ReviewerDecisions;
use super::Problem;
use crate::engine_discard::ENGINE_RESTORED_CAPTURE;
use crate::onboard::Finding;
use crate::report_export::decisions_file::{DecisionRowJson, DECISIONS_FILE};

/// One problem per merged row that has no decision record naming it, that state and a reason,
/// and one per site a live group decision covers that has no per-site record. Read from the
/// package's own files: the merged rows of `findings.json` and the records of `decisions.json`.
/// A merged row is one whose `merged_into` names the row it points to.
pub fn merged_without_decision_in_archive(
    entries: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Vec<Problem> {
    let mut problems = Vec::new();
    // A check that cannot read its evidence refuses (fail closed): an absent, unparseable or
    // malformed file is one problem naming it, never "no merged rows, nothing to check".
    let unreadable = |file: &str, why: &str| {
        Problem::new(
            file,
            "merge check",
            format!(
                "the merge check cannot read {file} ({why}), so it cannot confirm that every \
                 merged row has its decision. Rebuild the package"
            ),
            None,
        )
    };
    let rows: Vec<serde_json::Value> = match entries.get("findings.json") {
        None => {
            problems.push(unreadable("findings.json", "it is absent"));
            Vec::new()
        }
        Some(bytes) => match serde_json::from_slice::<serde_json::Value>(bytes) {
            Err(_) => {
                problems.push(unreadable("findings.json", "it does not parse"));
                Vec::new()
            }
            Ok(v) => match v["findings"].as_array() {
                Some(rows) => rows.clone(),
                None => {
                    problems.push(unreadable("findings.json", "its findings are not a list"));
                    Vec::new()
                }
            },
        },
    };
    let records: Vec<DecisionRowJson> = match entries.get(DECISIONS_FILE) {
        None => {
            problems.push(unreadable(DECISIONS_FILE, "it is absent"));
            Vec::new()
        }
        Some(bytes) => match serde_json::from_slice::<serde_json::Value>(bytes) {
            Err(_) => {
                problems.push(unreadable(DECISIONS_FILE, "it does not parse"));
                Vec::new()
            }
            Ok(v) if !v["decisions"].is_array() => {
                problems.push(unreadable(DECISIONS_FILE, "its decisions are not a list"));
                Vec::new()
            }
            Ok(v) => match serde_json::from_value(v["decisions"].clone()) {
                Ok(records) => records,
                Err(_) => {
                    problems.push(unreadable(DECISIONS_FILE, "a decision record is malformed"));
                    Vec::new()
                }
            },
        },
    };

    for row in &rows {
        let target = row["merged_into"].as_str().unwrap_or_default();
        if target.is_empty() {
            continue;
        }
        let id = row["site_id"].as_str().unwrap_or_default();
        if !merge_is_backed(&records, id, target) {
            problems.push(Problem::new(
                "findings.json",
                format!(
                    "{} line {}",
                    row["path"].as_str().unwrap_or_default(),
                    row["line"]
                ),
                format!(
                    "this finding exports as \"{}\" but decisions.json holds no merge decision \
                     with a reason for it. Merge it again, with the reason",
                    row["disposition_label"]
                        .as_str()
                        .or_else(|| row["status"].as_str())
                        .unwrap_or("merged")
                ),
                row["rule_id"].as_str().map(str::to_string),
            ));
        }
    }

    // Every live group decision has one per-site record for each site it covers. A per-site
    // record the reviewer revoked on its own (an undo of one site of a batch) is still a record:
    // the site was accounted for, and it is simply undecided again.
    for gap in group_decision_gaps(&records) {
        problems.push(Problem::new(
            DECISIONS_FILE,
            format!("group decision {} on {}", gap.decision_id, gap.node_id),
            format!(
                "the group decision covers a site with no record of its own for it ({}). \
                 Decide the group again so every site it covers holds the decision",
                gap.site_id
            ),
            None,
        ));
    }
    problems
}

/// Whether a merged row (the row `site_id`, merged into `target`) has the decision behind it:
/// a live site-scope record naming that row, the state `merged_into`, that target, and a reason.
/// The archive check and the review screen's export panel both ask this question here.
pub(crate) fn merge_is_backed(records: &[DecisionRowJson], site_id: &str, target: &str) -> bool {
    records.iter().any(|d| {
        !d.revoked
            && d.scope == "site"
            && d.site_id == site_id
            && d.state == "merged_into"
            && d.merged_into == target
            && d.has_reason
    })
}

/// One site a live group decision covers that has no per-site record of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GroupDecisionGap {
    pub decision_id: String,
    pub node_id: String,
    /// The site's id ([`crate::report_export::site_id`]).
    pub site_id: String,
}

/// Every site a live group decision covers that holds no per-site record, in record order.
pub(crate) fn group_decision_gaps(records: &[DecisionRowJson]) -> Vec<GroupDecisionGap> {
    let mut gaps = Vec::new();
    for node in records.iter().filter(|d| d.scope == "node" && !d.revoked) {
        for covered in &node.applies_to {
            let has_record = records
                .iter()
                .any(|d| d.scope == "site" && d.inherited_from == node.id && d.site_id == *covered);
            if !has_record {
                gaps.push(GroupDecisionGap {
                    decision_id: node.id.clone(),
                    node_id: node.node_id.clone(),
                    site_id: covered.clone(),
                });
            }
        }
    }
    gaps
}

/// The rows the reviewer restored after the engine threw them out that `decisions` has not
/// decided since: what [`restored_without_decision`] refuses, as rows.
pub fn undecided_restored<'a>(
    findings: &'a [Finding],
    decisions: &'a dyn ReviewerDecisions,
) -> impl Iterator<Item = &'a Finding> {
    findings
        .iter()
        .filter(|f| f.captures.contains_key(ENGINE_RESTORED_CAPTURE))
        .filter(|f| !decisions.has_decision(f))
}

/// One problem per row the reviewer restored after the engine threw it out that `decisions` has
/// not decided since.
pub fn restored_without_decision(
    findings: &[Finding],
    decisions: &dyn ReviewerDecisions,
) -> Vec<Problem> {
    undecided_restored(findings, decisions)
        .map(|f| {
            Problem::new(
                "findings.json",
                format!("{} line {}", f.path, f.line),
                "the engine threw this finding out and the reviewer put it back, and no reviewer \
                 decision is recorded for it since. Open it, then confirm it, correct it or mark \
                 it a false positive",
                Some(f.rule_id.clone()),
            )
        })
        .collect()
}
