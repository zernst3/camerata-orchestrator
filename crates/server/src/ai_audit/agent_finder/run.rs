//! The agent finder's runner: one piece end to end, send again, the follow-up, joints,
//! conventions from answers, severity steps, failed parts, checkpoint and resume. Build 3 fills
//! in the body of [`find_repo`]; its signature is the contract with the call site.

use super::coverage::PieceCoverage;
use super::PieceAgent;
use crate::ai_audit::{FailedPass, RuleRecommendation, UsageMeter};
use crate::cost_cap::RunControls;
use crate::onboard::{Finding, ProposedRule};
use crate::scan_ledger::StageSample;
use crate::scan_routing::AuditRule;

/// Settings of one finder run that are not on [`RunControls`]. Build 3 may add fields.
#[derive(Debug, Clone, PartialEq)]
pub struct FinderOptions {
    /// The finder's model (resolved: the explicit pick, else [`super::DEFAULT_FINDER_MODEL`]).
    pub model: String,
    /// Internal runs only: split every unit into at least this many pieces.
    pub force_pieces: Option<usize>,
    /// Whether rules' declared `applies_to` scopes route files to rules.
    pub applies_to_routing: bool,
    /// Pieces in flight at once (3 on the API, 1 on the command line backend).
    pub pieces_in_flight: usize,
}

/// A rule the finder could not complete for a unit, so the unit's answer for it is partial.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialRule {
    pub unit: String,
    pub rule_id: String,
    pub reason: String,
}

/// What the finder returns beyond the five values `audit_repo` returns.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FinderExtras {
    /// One coverage record for each piece.
    pub coverage: Vec<PieceCoverage>,
    /// Rules recorded as not completed for a unit.
    pub partial_rules: Vec<PartialRule>,
}

/// The same five values as `ai_audit::audit_repo` plus [`FinderExtras`].
pub type FinderResult = (
    Vec<Finding>,
    Vec<ProposedRule>,
    Vec<RuleRecommendation>,
    Vec<FailedPass>,
    Vec<StageSample>,
    FinderExtras,
);

/// Find the problems of one repository with the agent finder.
#[allow(dead_code, clippy::too_many_arguments)]
pub async fn find_repo(
    agent: &dyn PieceAgent,
    repo: &str,
    files: &[(String, String)],
    selected: &[AuditRule],
    corpus: &camerata_rules::RuleSet,
    controls: &RunControls,
    options: &FinderOptions,
    meter: Option<&UsageMeter>,
) -> anyhow::Result<FinderResult> {
    let _ = (
        agent, repo, files, selected, corpus, controls, options, meter,
    );
    unimplemented!("build 3")
}
