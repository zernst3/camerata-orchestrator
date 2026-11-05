//! A site of the tree as the old [`FindingView`] (spec rule 4: reuse, do not reinvent).
//!
//! The code viewer (`O`) and the stacked view (`B`) were built for the flat Queue and read a
//! [`FindingView`], keyed by [`finding_key`](crate::triage::finding_key). A site row carries the
//! same facts and the server's own site key, so the two keys are the same string and a decision
//! the viewer makes on a finding is a decision on the site.

use std::collections::{BTreeMap, HashMap};

use camerata_api_types::decisions::DecisionState;
use camerata_api_types::review_view::SiteRow;

use crate::triage::{Disposition, FindingView, TechDebtBucket, TriageState};

/// The words between a headline and its explanation in the old view's `detail`.
const HEADLINE_JOIN: &str = " - ";

/// `row` as the finding the viewer and the stack read.
pub fn finding_view_of(row: &SiteRow) -> FindingView {
    let detail = match (row.headline.trim(), row.detail.trim()) {
        ("", d) => d.to_string(),
        (h, "") => h.to_string(),
        (h, d) => format!("{h}{HEADLINE_JOIN}{d}"),
    };
    FindingView {
        repo: row.repo.clone(),
        path: row.path.clone(),
        line: row.line,
        rule_id: row.rule_id.clone(),
        severity: row.severity.clone(),
        snippet: row.snippet.clone(),
        detail,
        status: "active".to_string(),
        also_matches: Vec::new(),
        preview: false,
        preview_tool: None,
        in_test: false,
        needs_review: false,
        confidence: row.stage1.confidence.clone(),
        effort: row.stage1.effort.clone(),
        evaluated_option_id: None,
        also_locations: Vec::new(),
        captures: BTreeMap::new(),
        category: row.category.clone(),
        kind: row.kind.clone(),
        site_key: row.site_key.clone(),
    }
}

/// Every row as findings, in the order given.
pub fn finding_views<'a>(rows: impl IntoIterator<Item = &'a SiteRow>) -> Vec<FindingView> {
    rows.into_iter().map(finding_view_of).collect()
}

/// The triage state a decision stands for.
pub fn triage_state_of(state: &DecisionState) -> TriageState {
    match state {
        DecisionState::Confirmed => TriageState::Confirmed,
        DecisionState::Ignored => TriageState::Ignored,
        DecisionState::FalsePositive => TriageState::FalsePositive,
        DecisionState::TechDebtNow | DecisionState::TechDebtLater => TriageState::TechDebt,
        DecisionState::MergedInto(_) => TriageState::MergedInto,
        // A restore is not a ruling: the row is still undecided.
        DecisionState::EngineRestored => TriageState::Unresolved,
    }
}

/// The decided rows as the viewer's disposition map, by site key. A row with no decision is
/// absent (Unresolved), as the map has always meant.
pub fn dispositions_of<'a>(
    rows: impl IntoIterator<Item = &'a SiteRow>,
) -> HashMap<String, Disposition> {
    rows.into_iter()
        .filter_map(|row| {
            let d = row.decision.as_ref()?;
            let state = triage_state_of(&d.state);
            (state != TriageState::Unresolved).then(|| {
                (
                    row.site_key.clone(),
                    Disposition {
                        state,
                        reason: d.reason.clone(),
                        bucket: if d.state == DecisionState::TechDebtNow {
                            TechDebtBucket::Now
                        } else {
                            TechDebtBucket::Later
                        },
                    },
                )
            })
        })
        .collect()
}
