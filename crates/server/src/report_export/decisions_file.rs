//! The package's `decisions.json` and Decisions sheet (design 5.3 and WO9).
//!
//! Every decision record the report was reviewed with, as the client package states it: which
//! site or tree node it is on, what was ruled, which sites a group decision covers and which
//! per-site records it wrote. The release gate reads this file, not the records the package was
//! built from, to check that a merge has its decision and a group decision has one record per
//! site ([`crate::release_gate::held`]).
//!
//! Reviewer text stays in the store. A record says only THAT a reason was recorded
//! (`has_reason`); the reason itself is printed where the client already sees it: a false
//! positive's on the False Positives sheet, and an accepted risk's once the client has confirmed
//! it ("Accepted risk: <reason>", design 5.4). A reviewer's name and the time are never printed.

use camerata_api_types::decisions::{Decision, DecisionState, Scope};
use serde::{Deserialize, Serialize};

use super::{site_id, ReviewedReport};

/// One decision record as the package prints it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionRowJson {
    pub id: String,
    /// `site` or `node`.
    pub scope: String,
    /// The site's id ([`site_id`]); empty on a node record.
    pub site_id: String,
    /// The tree node a group decision was made on; empty on a site record.
    pub node_id: String,
    /// `confirmed`, `ignored`, `false_positive`, `tech_debt_now`, `tech_debt_later`,
    /// `merged_into` or `engine_restored`.
    pub state: String,
    /// The [`site_id`] of the row a merge points to; empty for any other state.
    pub merged_into: String,
    /// Node record: the [`site_id`] of every site the decision covers.
    pub applies_to: Vec<String>,
    /// The id of the group decision a per-site record was written by; empty otherwise.
    pub inherited_from: String,
    /// A reason was recorded.
    pub has_reason: bool,
    /// The reason, only where the client already sees it; empty otherwise.
    pub reason: String,
    pub confirmed_by_client: bool,
    /// The record was undone. It stays in the file: the record that was undone is part of the
    /// history of the ruling.
    pub revoked: bool,
}

/// The state's name in the file.
pub fn state_name(state: &DecisionState) -> &'static str {
    match state {
        DecisionState::Confirmed => "confirmed",
        DecisionState::Ignored => "ignored",
        DecisionState::FalsePositive => "false_positive",
        DecisionState::TechDebtNow => "tech_debt_now",
        DecisionState::TechDebtLater => "tech_debt_later",
        DecisionState::MergedInto(_) => "merged_into",
        DecisionState::EngineRestored => "engine_restored",
    }
}

fn row_of(d: &Decision) -> DecisionRowJson {
    let confirmed_by_client = d.confirmed_by_client.is_some();
    let (scope, site, node) = match &d.scope {
        Scope::Site => ("site", site_id(&d.key), String::new()),
        Scope::Node(node) => ("node", String::new(), node.clone()),
    };
    // The reasons the client already reads: a false positive's, and an accepted risk's once the
    // client has confirmed it. A ruling the reviewer undid is no longer one the client reads: its
    // reason is reviewer text for nothing, so the record keeps `has_reason` and prints none.
    let shown = match &d.state {
        DecisionState::FalsePositive => true,
        DecisionState::Ignored => confirmed_by_client,
        _ => false,
    };
    DecisionRowJson {
        id: d.id.clone(),
        scope: scope.to_string(),
        site_id: site,
        node_id: node,
        state: state_name(&d.state).to_string(),
        merged_into: match &d.state {
            DecisionState::MergedInto(target) => site_id(target),
            _ => String::new(),
        },
        applies_to: d.applies_to.iter().map(|k| site_id(k)).collect(),
        inherited_from: d.inherited_from.clone().unwrap_or_default(),
        has_reason: !d.reason.trim().is_empty(),
        reason: if shown && d.is_active() {
            d.reason.trim().to_string()
        } else {
            String::new()
        },
        confirmed_by_client,
        revoked: !d.is_active(),
    }
}

/// Every record `reviewed` was reviewed with, revoked ones included, in the order they were
/// written.
pub fn decision_rows(reviewed: &ReviewedReport) -> Vec<DecisionRowJson> {
    reviewed.all_records().iter().map(row_of).collect()
}

/// The file's path in the package.
pub const DECISIONS_FILE: &str = "decisions.json";

/// The bytes of `decisions.json`.
pub fn decisions_json(reviewed: &ReviewedReport) -> anyhow::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(&serde_json::json!({
        "decisions": decision_rows(reviewed),
    }))?;
    bytes.push(b'\n');
    Ok(bytes)
}
