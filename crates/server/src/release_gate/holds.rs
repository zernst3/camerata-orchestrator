//! What the release gate refuses, as rows: the review screen's export panel (review UI spec
//! section 10).
//!
//! The gate's reviewer-decision checks each answer one question about a reviewed report: which
//! rows are held for a ruling nobody has made? Each check owns the filter that answers it
//! ([`super::evidence::undecided_unverified`], [`super::junk::undecided_junk`],
//! [`super::held::undecided_restored`], [`super::engine_check::uncorrected_no`] and
//! [`super::engine_check::uncorrected_no_thrown_out`],
//! [`super::held::merge_is_backed`] and [`super::held::group_decision_gaps`]) and turns its rows into [`super::Problem`]s. This module
//! turns the same rows into an [`ExportGateView`], so the panel lists exactly what the gate
//! refuses for the same reviewed report and cannot drift from it. It adds no rule of its own.
//!
//! The Engine Check's pending No is here too, on a kept row and on a thrown-out one (the export
//! waits for the reviewer to put a thrown-out row back or to confirm the throw-out):
//! [`super::engine_check`] reads the evidence attached to the reviewed report
//! ([`ReviewedReport::engine_check`]). An Engine Check file that cannot be
//! read lists no row (the gate refuses with a problem naming the file instead).

use std::collections::HashMap;

use camerata_api_types::review_view::{ExportGateView, HeldRow, HoldReason, NodeDecisionGap};

use super::{engine_check, evidence, held, junk};
use crate::onboard::Finding;
use crate::report_export::decisions_file::decision_rows;
use crate::report_export::overlay::merged_target;
use crate::report_export::{site_id, ReviewedReport};

/// A held row as the panel lists it.
pub fn held_row(f: &Finding, reason: HoldReason, detail: impl Into<String>) -> HeldRow {
    HeldRow {
        key: f.site_key.clone(),
        path: f.path.clone(),
        line: f.line,
        rule_id: f.rule_id.clone(),
        reason,
        detail: detail.into(),
        node_id: f.group_path.get(2).cloned().unwrap_or_default(),
    }
}

/// The hold sets of one sealed, overlaid report (WO16d F1). There is one function for "which
/// rows are held": every reader (the shell's Held count, `/reviewed-report`, the export panel)
/// and every writer (the node decision's left-out list) takes its answer from here, computed from
/// the report the release gate reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HoldSet {
    /// The rows waiting for a ruling nobody has made: exactly the rows the release gate refuses
    /// (junk, unverified evidence, restored without a decision, an uncorrected Engine Check No,
    /// merged without a decision), by site key. The shell's count is this set's size.
    pub awaiting_a_ruling: std::collections::BTreeSet<String>,
    /// The rows a batch decision must leave alone: [`Self::awaiting_a_ruling`] plus the rows a
    /// ruling already stands on, which a group's inherited record would overwrite (a merge the
    /// reviewer recorded, an evidence or junk row they ruled on): the reasons in
    /// [`crate::decisions::held_keys`], read from the sealed report.
    pub kept_out_of_a_batch: std::collections::HashSet<String>,
}

/// The hold sets for `reviewed`. Nothing here is a rule of its own: the gate's rows come from
/// [`gate_holds`] and the reasons a batch leaves a row alone from [`crate::decisions::held_keys`].
pub fn hold_set(reviewed: &ReviewedReport) -> HoldSet {
    let gate = gate_holds(reviewed);
    let awaiting_a_ruling: std::collections::BTreeSet<String> = gate
        .held
        .iter()
        .chain(&gate.merged_without_decision)
        .map(|h| h.key.clone())
        .collect();
    let mut kept_out_of_a_batch = crate::decisions::held_keys(
        &reviewed.report().findings,
        reviewed.records(),
        reviewed.engine_check(),
    );
    // A decision may name a thrown-out node, whose rows are in `discarded` and not in `findings`.
    kept_out_of_a_batch.extend(crate::decisions::held_keys(
        &reviewed.report().discarded,
        reviewed.records(),
        reviewed.engine_check(),
    ));
    kept_out_of_a_batch.extend(awaiting_a_ruling.iter().cloned());
    HoldSet {
        awaiting_a_ruling,
        kept_out_of_a_batch,
    }
}

/// Every row the release gate refuses for `reviewed`, by kind. Rows are in the order of the
/// report's rows within each kind, kinds in the order the gate runs its checks.
pub fn gate_holds(reviewed: &ReviewedReport) -> ExportGateView {
    let findings = &reviewed.report().findings;
    let discarded = &reviewed.report().discarded;

    let mut rows: Vec<HeldRow> = Vec::new();
    rows.extend(
        junk::undecided_junk(findings, reviewed).map(|f| held_row(f, HoldReason::JunkContent, "")),
    );
    rows.extend(
        evidence::undecided_unverified(findings, reviewed)
            .map(|f| held_row(f, HoldReason::EvidenceUnverified, "")),
    );
    rows.extend(
        held::undecided_restored(findings, reviewed)
            .map(|f| held_row(f, HoldReason::RestoredWithoutDecision, "")),
    );
    rows.extend(
        engine_check::uncorrected_no(findings, reviewed)
            .into_iter()
            .map(|f| held_row(f, HoldReason::EngineCheckNoWithoutCorrection, "")),
    );
    rows.extend(
        engine_check::uncorrected_no_thrown_out(discarded, reviewed)
            .into_iter()
            .map(|f| {
                held_row(
                    f,
                    HoldReason::EngineCheckNoWithoutCorrection,
                    "Engine Check said No to throwing it out, no correction yet",
                )
            }),
    );

    // Merges and group decisions are checked against the decision records the package prints.
    let records = decision_rows(reviewed);
    let by_key: HashMap<&str, &Finding> =
        findings.iter().map(|f| (f.site_key.as_str(), f)).collect();
    let merged_without_decision = findings
        .iter()
        .filter_map(|f| merged_target(f).map(|target| (f, target)))
        .filter(|(f, target)| {
            !held::merge_is_backed(&records, &site_id(&f.site_key), &site_id(target))
        })
        .map(|(f, target)| {
            let label = by_key
                .get(target)
                .map_or_else(String::new, |t| format!("{}:{}", t.path, t.line));
            held_row(f, HoldReason::MergedWithoutDecision, label)
        })
        .collect();

    let by_site_id: HashMap<String, &str> = findings
        .iter()
        .map(|f| (site_id(&f.site_key), f.site_key.as_str()))
        .collect();
    let mut gaps: Vec<NodeDecisionGap> = Vec::new();
    for gap in held::group_decision_gaps(&records) {
        let key = by_site_id
            .get(&gap.site_id)
            .map_or_else(|| gap.site_id.clone(), |k| (*k).to_string());
        match gaps.iter_mut().find(|g| g.decision_id == gap.decision_id) {
            Some(existing) => existing.missing.push(key),
            None => gaps.push(NodeDecisionGap {
                decision_id: gap.decision_id,
                node_id: gap.node_id,
                missing: vec![key],
            }),
        }
    }

    ExportGateView {
        held: rows,
        merged_without_decision,
        node_decisions_missing_members: gaps,
    }
}
