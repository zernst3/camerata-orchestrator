//! The agent's own rating of its problems (design section 5): its `sure`, `effort` and a category
//! taken from its label go through the same code a calibration verdict goes through
//! ([`apply_verdicts`]). An `unsure` problem is put under needs a decision; the agent never says
//! "not real", it simply does not report.

use super::super::reply::{Problem, FINDER_EFFORT_CAPTURE, FINDER_SURE_CAPTURE};
use crate::ai_audit::apply_verdicts;
use crate::onboard::Finding;

/// The note stored with the rating. Operator side: it says the rating came from the finder and
/// that no second reviewer rated the problem.
pub const RATING_NOTE: &str =
    "Rated by the review agent that found it; no second reviewer rated this problem.";

/// A category from the agent's label, when the label names one of the known categories.
fn category_of(label: &str) -> Option<String> {
    let slug = label.trim().to_ascii_lowercase().replace([' ', '_'], "-");
    crate::ai_audit::is_known_category(&slug).then_some(slug)
}

/// Apply the agent's rating to `findings` (the findings of `problems`, in the same order).
pub(super) fn apply_agent_rating(findings: Vec<Finding>, problems: &[Problem]) -> Vec<Finding> {
    let aligned = findings.len() == problems.len();
    let verdicts: Vec<serde_json::Value> = findings
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let unsure = f.captures.get(FINDER_SURE_CAPTURE).map(String::as_str) == Some("unsure");
            let effort = f.captures.get(FINDER_EFFORT_CAPTURE).cloned();
            let category = if aligned {
                category_of(&problems[i].label)
            } else {
                None
            };
            serde_json::json!({
                "index": i,
                "severity": f.severity,
                "real": if unsure { "unclear" } else { "yes" },
                "effort": effort,
                "category": category,
                "reason": RATING_NOTE,
            })
        })
        .collect();
    apply_verdicts(
        &serde_json::json!({ "verdicts": verdicts }).to_string(),
        findings,
    )
}
