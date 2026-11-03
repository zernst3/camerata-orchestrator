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
    let findings: Option<serde_json::Value> = entries
        .get("findings.json")
        .and_then(|b| serde_json::from_slice(b).ok());
    let records: Vec<DecisionRowJson> = entries
        .get(DECISIONS_FILE)
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|v| serde_json::from_value(v["decisions"].clone()).ok())
        .unwrap_or_default();

    let rows = findings
        .as_ref()
        .and_then(|f| f["findings"].as_array())
        .cloned()
        .unwrap_or_default();
    for row in &rows {
        let target = row["merged_into"].as_str().unwrap_or_default();
        if target.is_empty() {
            continue;
        }
        let id = row["site_id"].as_str().unwrap_or_default();
        let backed = records.iter().any(|d| {
            !d.revoked
                && d.scope == "site"
                && d.site_id == id
                && d.state == "merged_into"
                && d.merged_into == target
                && d.has_reason
        });
        if !backed {
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
    for node in records.iter().filter(|d| d.scope == "node" && !d.revoked) {
        for covered in &node.applies_to {
            let has_record = records
                .iter()
                .any(|d| d.scope == "site" && d.inherited_from == node.id && d.site_id == *covered);
            if !has_record {
                problems.push(Problem::new(
                    DECISIONS_FILE,
                    format!("group decision {} on {}", node.id, node.node_id),
                    format!(
                        "the group decision covers a site with no record of its own for it ({covered}). \
                         Decide the group again so every site it covers holds the decision"
                    ),
                    None,
                ));
            }
        }
    }
    problems
}

/// One problem per row the reviewer restored after the engine threw it out that `decisions` has
/// not decided since.
pub fn restored_without_decision(
    findings: &[Finding],
    decisions: &dyn ReviewerDecisions,
) -> Vec<Problem> {
    findings
        .iter()
        .filter(|f| f.captures.contains_key(ENGINE_RESTORED_CAPTURE))
        .filter(|f| !decisions.has_decision(f))
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
