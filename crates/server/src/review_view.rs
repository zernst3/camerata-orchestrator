//! The review read model's composer (review UI spec sections 4 to 10; work order WO9c).
//!
//! [`ReviewComposer`] takes the reviewed report (the sealed report with the reviewer's overlay
//! applied, [`crate::report_export::apply_overlay`]) and the stores' state, and returns the typed
//! views of [`camerata_api_types::review_view`]: the shell, the tree, one node's sites, the queue,
//! the Engine Check lane, the export gate and the settings. It is pure: no model call, no write,
//! no clock. The routes ([`crate::routes`]'s `review_routes`) load the inputs and hand them over.
//!
//! # One source
//!
//! Every view is built from the [`ReviewedReport`] and nothing else, the rule the exports follow
//! (design WO9). A screen therefore cannot show a row, a decision or a count the exports do not.
//! The export gate view in particular is derived from the release gate's own refusal functions
//! ([`crate::release_gate::holds`]), not from a second copy of its rules.
//!
//! # The line
//!
//! The escalation line is marked from the stored candidates and the project's CURRENT settings
//! ([`camerata_api_types::review::re_mark`], design A4) once, where the report is loaded
//! ([`crate::report_export::re_mark_line`]), so a changed rate or floor shows on the next read AND
//! in the next export, with no model call. The composer reads the marks it is given. "Show below the line" is a view flag on the queue request
//! and changes nothing ([`ReviewComposer::queue`], spec U5).
//!
//! # Size
//!
//! [`ReviewComposer::tree`] never lists a site: its size follows the number of nodes. Sites are
//! read one node at a time, 500 at a time ([`ReviewComposer::sites_page`], spec U1). Building the
//! index and the tree is linear in the rows and nodes (a 30,000 site run builds in well under two
//! seconds in a debug build; `tests/review_contracts.rs` records the numbers).

use std::collections::{HashMap, HashSet};

use camerata_api_types::decisions::{Decision, DecisionState};
use camerata_api_types::edits::Edit;
use camerata_api_types::project::{ReviewSettings, STAGE2_DEFAULT_MODEL};
use camerata_api_types::review::{
    re_mark, EscalationBudget, EscalationCandidate, EscalationReason,
};
use camerata_api_types::review_view::{
    EngineCheckSettingsView, EngineCheckVerdict, EscalationBudgetOutcome, EscalationBudgetReq,
    EscalationSettingsView, ExportGateView, HoldReason, LaneCounts, MovedFromView, ReviewCounts,
    ReviewSettingsView, ReviewShellView, SettingsLimits, StageModelsView, StaleSummary,
    ESCALATION_FLOOR_MAX, ESCALATION_RATE_MAX, SAMPLE_SIZE_MAX, SAMPLE_SIZE_SUGGESTED,
};

use crate::decisions::{apply_to, AppliedDecisions};
use crate::onboard::{Finding, ScanReport};
use crate::report_export::overlay::StaleEditNote;
use crate::report_export::ReviewedReport;
use crate::review_tree::{KeyLink, LinkHow, ReviewTree};

mod lane;
mod queue;
mod rows;
mod tree;

#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod tests;

pub use tree::{decode_cursor, encode_cursor, severity_rank};

/// Everything a view is composed from. Borrowed: the composer copies nothing it does not return.
pub struct ReviewInputs<'a> {
    /// The inspection (entry id) the views are about.
    pub run_id: &'a str,
    pub engine_version: &'a str,
    pub reviewed: &'a ReviewedReport,
    /// The live (not discarded) edits of the project, oldest first.
    pub edits: &'a [Edit],
    /// The project's review settings, as they stand now.
    pub settings: &'a ReviewSettings,
    /// The scan model, the calibration model and the fix model; the group review model is read
    /// from `settings`.
    pub scan_model: &'a str,
    pub calibration_model: &'a str,
    pub fix_model: &'a str,
    /// The Engine Check verdicts given on this run, the latest per site.
    pub verdicts: &'a [EngineCheckVerdict],
    /// The sites that moved since an earlier run a decision or an edit was made on: new key to
    /// where it was.
    pub moved: &'a HashMap<String, MovedFromView>,
    /// The same moves as key links, for the stale-decision standing.
    pub links: &'a [KeyLink],
}

/// The composer: indexes built once from [`ReviewInputs`], then one method per view.
pub struct ReviewComposer<'a> {
    inputs: &'a ReviewInputs<'a>,
    report: &'a ScanReport,
    tree: Option<&'a ReviewTree>,
    /// Every row by site key, kept rows and thrown-out rows.
    rows: HashMap<&'a str, &'a Finding>,
    /// Node id to its position in the tree's node list.
    node_at: HashMap<&'a str, usize>,
    /// Node id to its children's positions, in tree order.
    children: HashMap<&'a str, Vec<usize>>,
    /// What the decisions say about each site of the run.
    standings: AppliedDecisions,
    /// The live decision records by id.
    records: HashMap<&'a str, &'a Decision>,
    /// The stored candidates marked again with the project's current settings.
    candidates: Vec<EscalationCandidate>,
    budget: EscalationBudget,
    candidate_at: HashMap<String, usize>,
    /// Sites any node lists as an outlier.
    outliers: HashSet<&'a str>,
    /// Edits by site key.
    edits_by_key: HashMap<&'a str, Vec<&'a Edit>>,
    /// Edits the overlay did not apply, by edit id.
    stale_edits: HashMap<&'a str, &'a StaleEditNote>,
    /// The rows the release gate refuses, and why (the first reason a row has).
    gate: ExportGateView,
    held: HashMap<String, HoldReason>,
    verdicts: HashMap<&'a str, &'a EngineCheckVerdict>,
}

impl<'a> ReviewComposer<'a> {
    /// Index the inputs. Linear in the rows, the nodes and the records.
    pub fn new(inputs: &'a ReviewInputs<'a>) -> Self {
        let report = inputs.reviewed.report().as_scan_report();
        let tree = report.tree.as_ref();
        let mut rows: HashMap<&str, &Finding> =
            HashMap::with_capacity(report.findings.len() + report.discarded.len());
        for f in report.discarded.iter().chain(&report.findings) {
            rows.insert(f.site_key.as_str(), f);
        }

        let mut node_at = HashMap::new();
        let mut children: HashMap<&str, Vec<usize>> = HashMap::new();
        let mut outliers = HashSet::new();
        if let Some(t) = tree {
            for (i, n) in t.nodes.iter().enumerate() {
                node_at.insert(n.id.as_str(), i);
                if let Some(p) = n.parent.as_deref() {
                    children.entry(p).or_default().push(i);
                }
                outliers.extend(n.outliers.iter().map(|o| o.key.as_str()));
            }
        }

        let records: HashMap<&str, &Decision> = inputs
            .reviewed
            .records()
            .iter()
            .map(|d| (d.id.as_str(), d))
            .collect();
        let standings = match tree {
            Some(t) => apply_to(t, inputs.reviewed.records(), inputs.links),
            None => AppliedDecisions::default(),
        };

        // The line as the report carries it: the loader marked it from the project's settings
        // before the overlay ([`crate::report_export::re_mark_line`]), the same marks the exports
        // ship, so this reads them and moves nothing.
        let candidates = tree.map(|t| t.candidates.clone()).unwrap_or_default();
        let budget = tree.map_or_else(
            || {
                EscalationBudget::compute(
                    inputs.settings.escalation_rate_per_kloc,
                    inputs.settings.escalation_floor,
                    report.code_lines,
                )
            },
            |t| t.budget,
        );
        let candidate_at = candidates
            .iter()
            .enumerate()
            .map(|(i, c)| (c.key.clone(), i))
            .collect();

        let stale_edits: HashMap<&str, &StaleEditNote> = inputs
            .reviewed
            .stale_edits()
            .iter()
            .map(|s| (s.edit.id.as_str(), s))
            .collect();
        // An edit belongs to the row of its own key. A stale edit whose site is no longer a row
        // belongs to the row the engine found that site moved to (the same old-to-new links the
        // decisions' stale standing reads), so the moved row shows it.
        let moved_to: HashMap<&str, &str> = inputs
            .links
            .iter()
            .filter(|l| l.how == LinkHow::Moved)
            .map(|l| (l.old_key.as_str(), l.new_key.as_str()))
            .collect();
        let mut edits_by_key: HashMap<&str, Vec<&Edit>> = HashMap::new();
        for e in inputs.edits {
            let own = e.key.as_str();
            let key = if stale_edits.contains_key(e.id.as_str()) && !rows.contains_key(own) {
                moved_to
                    .get(own)
                    .copied()
                    .filter(|new| rows.contains_key(new))
                    .unwrap_or(own)
            } else {
                own
            };
            edits_by_key.entry(key).or_default().push(e);
        }

        // What the release gate refuses, from its own refusal functions (the Engine Check's
        // pending No among them: the reviewed report carries the store's evidence).
        let gate = crate::release_gate::holds::gate_holds(inputs.reviewed);
        let mut held: HashMap<String, HoldReason> = HashMap::new();
        for h in gate.held.iter().chain(&gate.merged_without_decision) {
            held.entry(h.key.clone()).or_insert(h.reason);
        }
        let verdicts = inputs
            .verdicts
            .iter()
            .map(|v| (v.key.as_str(), v))
            .collect();

        Self {
            inputs,
            report,
            tree,
            rows,
            node_at,
            children,
            standings,
            records,
            candidates,
            budget,
            candidate_at,
            outliers,
            edits_by_key,
            stale_edits,
            gate,
            held,
            verdicts,
        }
    }

    /// The row with site key `key`, kept or thrown out.
    fn row(&self, key: &str) -> Option<&'a Finding> {
        self.rows.get(key).copied()
    }

    /// The escalation line as it stands under the project's current settings.
    pub fn budget(&self) -> &EscalationBudget {
        &self.budget
    }

    /// The stored candidates, marked again with the current settings, in global rank order.
    pub fn candidates(&self) -> &[EscalationCandidate] {
        &self.candidates
    }

    /// The candidate for a site, when the group reviewer wanted it escalated.
    fn candidate(&self, key: &str) -> Option<&EscalationCandidate> {
        self.candidate_at.get(key).map(|i| &self.candidates[*i])
    }

    fn group_reviewed(&self) -> bool {
        self.tree.is_some_and(|t| t.group_reviewed)
    }

    // ── section 4: the shell ──────────────────────────────────────────────────────────────

    /// `GET /api/projects/:id/review`.
    pub fn shell(&self) -> ReviewShellView {
        let mut counts = ReviewCounts {
            total: self.report.findings.len(),
            ..ReviewCounts::default()
        };
        let mut stale = StaleSummary {
            stale_edits: self.inputs.reviewed.stale_edits().len(),
            ..StaleSummary::default()
        };
        for f in &self.report.findings {
            let standing = self.standings.get(&f.site_key);
            match standing.and_then(|s| s.decision.as_ref()).map(|d| &d.state) {
                None => counts.unresolved += 1,
                Some(DecisionState::Confirmed) => counts.confirmed += 1,
                Some(DecisionState::Ignored) => counts.ignored += 1,
                Some(DecisionState::FalsePositive) => counts.false_positive += 1,
                Some(DecisionState::TechDebtNow | DecisionState::TechDebtLater) => {
                    counts.tech_debt += 1;
                }
                Some(DecisionState::MergedInto(_)) => counts.merged += 1,
                // A restore is not a ruling; the standing never carries one.
                Some(DecisionState::EngineRestored) => counts.unresolved += 1,
            }
            if let Some(s) = standing {
                stale.stale_decisions += usize::from(s.stale.is_some());
                stale.new_since_decision += usize::from(s.new_since_decision.is_some());
            }
            stale.moved += usize::from(self.inputs.moved.contains_key(&f.site_key));
        }
        counts.held = self.gate.rows_needing_a_decision();

        let sample = self.sample();
        ReviewShellView {
            run_id: self.inputs.run_id.to_string(),
            engine_version: self.inputs.engine_version.to_string(),
            counts,
            budget: self.budget,
            lanes: LaneCounts {
                tree_undecided: counts.unresolved,
                escalations: self.budget.escalated,
                engine_check: sample.drawn(),
                engine_check_off: self.inputs.settings.sample_size_per_run == 0,
                flat_rows: counts.total,
            },
            export_gate: self.gate.clone(),
            stale,
            group_reviewed: self.group_reviewed(),
        }
    }

    // ── section 10: the export gate ───────────────────────────────────────────────────────

    /// `GET /api/projects/:id/review/export-gate`.
    pub fn export_gate(&self) -> ExportGateView {
        self.gate.clone()
    }

    /// The rows the gate refuses, as the held lookup the row builder reads.
    fn hold_of(&self, key: &str) -> Option<HoldReason> {
        self.held.get(key).copied()
    }

    // ── section 9: settings ───────────────────────────────────────────────────────────────

    /// `GET /api/projects/:id/review/settings`.
    pub fn settings(&self) -> ReviewSettingsView {
        let s = self.inputs.settings;
        let group_review = if s.group_review_model.trim().is_empty() {
            STAGE2_DEFAULT_MODEL.to_string()
        } else {
            s.group_review_model.clone()
        };
        ReviewSettingsView {
            stage_models: StageModelsView {
                scan: self.inputs.scan_model.to_string(),
                calibration: self.inputs.calibration_model.to_string(),
                group_review,
                fix: self.inputs.fix_model.to_string(),
            },
            escalation: EscalationSettingsView {
                rate_per_kloc: s.escalation_rate_per_kloc,
                floor: s.escalation_floor,
                counted_lines: self.budget.counted_lines,
                budget: self.budget,
                over_budget_reasons: s.over_budget_reasons.clone(),
            },
            engine_check: EngineCheckSettingsView {
                sample_size_per_run: s.sample_size_per_run,
                suggested: SAMPLE_SIZE_SUGGESTED,
            },
            limits: SettingsLimits {
                rate_max: ESCALATION_RATE_MAX,
                floor_max: ESCALATION_FLOOR_MAX,
                sample_max: SAMPLE_SIZE_MAX,
            },
        }
    }

    /// The line before and after `req`, with no write: marks the stored candidates again under the
    /// proposed rate, floor and over-budget reasons ([`re_mark`]) and counts what crossed the line.
    /// `applied` is what the caller did with it; the composer never writes.
    pub fn budget_outcome(
        &self,
        req: &EscalationBudgetReq,
        applied: bool,
    ) -> EscalationBudgetOutcome {
        let mut candidates = self.tree.map(|t| t.candidates.clone()).unwrap_or_default();
        let mut after =
            EscalationBudget::compute(req.rate_per_kloc, req.floor, self.budget.counted_lines);
        re_mark(&mut candidates, &mut after, &req.over_budget_reasons);
        let (mut moved_above, mut moved_below) = (0, 0);
        for new in &candidates {
            let was = self.candidate(&new.key).is_some_and(|c| c.escalated);
            match (was, new.escalated) {
                (false, true) => moved_above += 1,
                (true, false) => moved_below += 1,
                _ => {}
            }
        }
        EscalationBudgetOutcome {
            applied,
            before: self.budget,
            after,
            moved_above,
            moved_below,
        }
    }

    /// The reasons that may exceed the budget under the current settings.
    fn over_budget_reasons(&self) -> Vec<EscalationReason> {
        self.inputs.settings.over_budget_reasons.clone()
    }
}
