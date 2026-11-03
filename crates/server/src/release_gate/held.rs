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
//! The package has no `decisions.json` yet (the Decisions sheet and file are a later work order),
//! so both checks read the live decision records the package was assembled from, in memory, the
//! way [`super::evidence`] reads the decisions. They are mechanical and hard.

use std::collections::{HashMap, HashSet};

use camerata_api_types::decisions::{live_by_key, Decision, DecisionState, Scope};

use super::evidence::ReviewerDecisions;
use super::Problem;
use crate::engine_discard::ENGINE_RESTORED_CAPTURE;
use crate::onboard::Finding;
use crate::report_export::overlay::{MERGED_INTO_CAPTURE, MERGED_LABEL_CAPTURE};

/// One problem per merged row that has no decision record naming it, that state and a reason,
/// and one per site a live group decision covers that has no per-site record.
pub fn merged_without_decision(findings: &[Finding], records: &[Decision]) -> Vec<Problem> {
    let live = live_by_key(records);
    let mut problems = Vec::new();

    for f in findings {
        let Some(target) = f.captures.get(MERGED_INTO_CAPTURE) else {
            continue;
        };
        let record = live.get(f.site_key.as_str()).and_then(|rs| {
            rs.iter().rev().find(|d| {
                matches!(&d.state, DecisionState::MergedInto(t) if t == target)
                    && !d.reason.trim().is_empty()
            })
        });
        if record.is_none() {
            problems.push(Problem::new(
                "findings.json",
                format!("{} line {}", f.path, f.line),
                format!(
                    "this finding exports as \"{}\" but no merge decision with a reason is \
                     recorded for it. Merge it again, with the reason",
                    f.captures
                        .get(MERGED_LABEL_CAPTURE)
                        .map_or("merged", String::as_str)
                ),
                Some(f.rule_id.clone()),
            ));
        }
    }

    // Every live group decision has one per-site record for each site it covers.
    let per_site: HashMap<&str, HashSet<&str>> = {
        let mut m: HashMap<&str, HashSet<&str>> = HashMap::new();
        // A per-site record the reviewer revoked on its own (an undo of one site of a batch) is
        // still a record: the site was accounted for, and it is simply undecided again.
        for d in records.iter().filter(|d| !d.is_node_record()) {
            if let Some(parent) = d.inherited_from.as_deref() {
                m.entry(parent).or_default().insert(d.key.as_str());
            }
        }
        m
    };
    for node in records
        .iter()
        .filter(|d| d.is_active() && d.is_node_record())
    {
        let have = per_site.get(node.id.as_str());
        let Scope::Node(node_id) = &node.scope else {
            continue;
        };
        for key in &node.applies_to {
            if !have.is_some_and(|keys| keys.contains(key.as_str())) {
                problems.push(Problem::new(
                    "decisions",
                    format!("group decision {} on {node_id}", node.id),
                    format!(
                        "the group decision covers a site with no record of its own for it ({}). \
                         Decide the group again so every site it covers holds the decision",
                        key.replace('\u{0}', " ")
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
