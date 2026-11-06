//! M4: a junk row does not ship without a reviewer's decision.
//!
//! `junk_finding::mark_rows` marks, when the report is sealed, every row that is real junk: a
//! placeholder, an empty explanation, the model's own refusal or chat reply, an untranslated JSON
//! fragment, a discard verdict. A reviewer might never open such a row, and a client would
//! receive it, so the row is held (`report_export::is_held_for_review`) and this check refuses
//! the export until the reviewer has decided it. It is the same shape as
//! [`super::evidence`]: it asks one question of a [`ReviewerDecisions`], so moving to the decision
//! store changes nothing here.
//!
//! Wording judgments (`junk_finding::WordingFlag`) never reach this check. They are review flags
//! on the row and refuse nothing.

use super::evidence::ReviewerDecisions;
use super::Problem;
use crate::junk_finding::JUNK_CONTENT_CAPTURE;
use crate::onboard::Finding;

/// The rows marked as junk that `decisions` has not decided: what [`junk_without_decision`]
/// refuses, as rows (the review screen's export panel lists these same rows).
pub fn undecided_junk<'a>(
    findings: &'a [Finding],
    decisions: &'a dyn ReviewerDecisions,
) -> impl Iterator<Item = &'a Finding> {
    findings
        .iter()
        .filter(|f| f.captures.contains_key(JUNK_CONTENT_CAPTURE))
        .filter(|f| !decisions.has_decision(f))
}

/// One problem per row marked as junk that `decisions` has not decided.
pub fn junk_without_decision(
    findings: &[Finding],
    decisions: &dyn ReviewerDecisions,
) -> Vec<Problem> {
    undecided_junk(findings, decisions)
        .map(|f| {
            Problem::new(
                "findings.json",
                format!("{} line {}", f.path, f.line),
                format!(
                    "this finding is junk ({}) and no reviewer decision is recorded for it. Open \
                     it, then confirm it, correct it or mark it a false positive",
                    f.captures
                        .get(JUNK_CONTENT_CAPTURE)
                        .map(String::as_str)
                        .unwrap_or("junk text"),
                ),
                Some(f.rule_id.clone()),
            )
        })
        .collect()
}
