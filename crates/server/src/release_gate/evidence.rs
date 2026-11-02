//! M5: an `evidence_unverified` row does not ship without a reviewer's decision.
//!
//! The evidence check (`onboard::evidence_check`) marks a row whose quoted snippet was not found
//! near its line. The row is kept and held, but a held row still reaches the package if nobody
//! looks at it. This check closes that: every marked row in the package must carry a decision
//! from the reviewer, and an export with any that does not is refused, naming the rows.
//!
//! # Where the decision comes from
//!
//! Today the reviewer's decisions are the dispositions in the export request (a map from a row's
//! key to its state). The review-tree design replaces them with a decision store; the check does
//! not care which. It asks one question of a [`ReviewerDecisions`]: has the reviewer decided this
//! row? Moving to the decision store is a new implementation of that trait and nothing else here
//! changes.

use std::collections::HashMap;

use super::Problem;
use crate::onboard::evidence_check::{
    EVIDENCE_UNVERIFIED_CAPTURE, EVIDENCE_UNVERIFIED_SITES_CAPTURE,
};
use crate::onboard::Finding;
use crate::report_export::DispositionWire;

/// The reviewer's decisions about rows, from whichever store holds them.
pub trait ReviewerDecisions {
    /// Whether the reviewer has decided `finding`: confirmed it, accepted it, scheduled it, or
    /// marked it a false positive. A row nobody has touched is not decided, and neither is one
    /// whose recorded state this build cannot read.
    fn has_decision(&self, finding: &Finding) -> bool;
}

/// The dispositions states that are a decision. These are the states `report_export::classify`
/// reads, plus `FalsePositive`, which the export partitions out before it classifies. An empty or
/// unrecognised state is "no decision", the same fail-soft reading the export gives it.
const DECISION_STATES: [&str; 4] = ["Confirmed", "Ignored", "TechDebt", "FalsePositive"];

impl ReviewerDecisions for HashMap<String, DispositionWire> {
    fn has_decision(&self, finding: &Finding) -> bool {
        self.get(&crate::report_export::finding_key(finding))
            .is_some_and(|d| DECISION_STATES.contains(&d.state.as_str()))
    }
}

/// One problem per row marked `evidence_unverified` that `decisions` has not decided.
pub fn unverified_without_decision(
    findings: &[Finding],
    decisions: &dyn ReviewerDecisions,
) -> Vec<Problem> {
    findings
        .iter()
        .filter(|f| f.captures.contains_key(EVIDENCE_UNVERIFIED_CAPTURE))
        .filter(|f| !decisions.has_decision(f))
        .map(|f| {
            Problem::new(
                "findings.json",
                format!("{} line {}", f.path, f.line),
                format!(
                    "the quoted evidence for this finding was not found in the file near the \
                     cited line ({}){}, and no reviewer decision is recorded for it. Open it, \
                     then confirm it, correct it or mark it a false positive",
                    f.captures
                        .get(EVIDENCE_UNVERIFIED_CAPTURE)
                        .map(String::as_str)
                        .unwrap_or("unverified"),
                    f.captures
                        .get(EVIDENCE_UNVERIFIED_SITES_CAPTURE)
                        .map(|sites| format!(
                            "; absorbed sites not verified: {}",
                            sites.replace('\n', ", ")
                        ))
                        .unwrap_or_default(),
                ),
                Some(f.rule_id.clone()),
            )
        })
        .collect()
}
