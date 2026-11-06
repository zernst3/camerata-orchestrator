//! The one-time migration of the UI draft's dispositions into the store (design 3.6).
//!
//! The draft keyed its dispositions by the line-based key the UI used before the site key
//! existed (`repo, rule, path, line, snippet`, with the advisory id appended for a dependency
//! row). Those keys are translated through the current rows to site keys. A disposition no row
//! matches is kept under its old key, for history, so blanking the draft loses nothing.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::{DecisionState, DraftDisposition};
use crate::onboard::Finding;

/// Who a migrated decision is recorded as made by.
pub const MIGRATED_BY: &str = "migrated from the review draft";

/// The key the UI draft used for a row before the site key: repo, rule, path, line and snippet
/// joined by NUL, with the advisory id appended for a dependency row.
pub fn legacy_finding_key(f: &Finding) -> String {
    let key = format!(
        "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        f.repo, f.rule_id, f.path, f.line, f.snippet
    );
    match f.captures.get(crate::dep_audit::ADVISORY_CAPTURE) {
        Some(advisory)
            if f.rule_id == crate::dep_audit::DEP_AUDIT_RULE_ID && !advisory.is_empty() =>
        {
            format!("{key}\u{0}{advisory}")
        }
        _ => key,
    }
}

/// The decision state a draft disposition means, or `None` for Unresolved and for a state this
/// build does not know (neither is a decision).
pub fn disposition_state(d: &DraftDisposition) -> Option<DecisionState> {
    match d.state.as_str() {
        "Confirmed" => Some(DecisionState::Confirmed),
        "Ignored" => Some(DecisionState::Ignored),
        "FalsePositive" => Some(DecisionState::FalsePositive),
        "TechDebt" if d.bucket == "Now" => Some(DecisionState::TechDebtNow),
        "TechDebt" => Some(DecisionState::TechDebtLater),
        _ => None,
    }
}

/// A draft's dispositions after translation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DraftTranslation {
    /// (current site key, disposition), one entry per row an old key named.
    pub matched: Vec<(String, DraftDisposition)>,
    /// (old key, disposition) for a key no current row matches.
    pub unmatched: Vec<(String, DraftDisposition)>,
    /// Entries that are not decisions.
    pub skipped: usize,
}

/// Translate the draft's dispositions through `rows` (which must carry their site keys). A key
/// that is already a current site key (a draft saved after the site key existed) is used as it
/// is. An old line-based key is translated to every row that had it: the old key could not tell
/// two identical rows apart, so the disposition it recorded covered both.
pub fn translate_draft(
    rows: &[Finding],
    draft: &BTreeMap<String, DraftDisposition>,
) -> DraftTranslation {
    let site_keys: HashSet<&str> = rows
        .iter()
        .map(|f| f.site_key.as_str())
        .filter(|k| !k.is_empty())
        .collect();
    let mut by_legacy: HashMap<String, Vec<&str>> = HashMap::new();
    for f in rows.iter().filter(|f| !f.site_key.is_empty()) {
        by_legacy
            .entry(legacy_finding_key(f))
            .or_default()
            .push(f.site_key.as_str());
    }
    let mut out = DraftTranslation::default();
    for (old, disposition) in draft {
        if disposition_state(disposition).is_none() {
            out.skipped += 1;
        } else if site_keys.contains(old.as_str()) {
            out.matched.push((old.clone(), disposition.clone()));
        } else if let Some(keys) = by_legacy.get(old) {
            for key in keys {
                out.matched.push(((*key).to_string(), disposition.clone()));
            }
        } else {
            out.unmatched.push((old.clone(), disposition.clone()));
        }
    }
    out
}
