//! The names of the INTERNAL-ONLY review-time data (work order `WORK_ORDER_REVIEW_TIME.md` item 2,
//! overridden by Zach's 2026-10-23 ruling): the review timer's per-group active time, the per-row
//! "reviewed" record, and the stats built from them.
//!
//! Zach's ruling: this data is never in any client export (xlsx, `findings.json`, the PDF, the
//! manifest, the CSV and JSON datasets, the README, anything a client receives). It is shown in an
//! internal stats view as data for pricing and nowhere else.
//!
//! WHICH GUARD IS WHICH. The primary guard is structural: the request that asks for a client
//! export (`export_product_zip`) carries the dispositions and the report options and nothing else
//! (a test pins the request body's keys), `FindingView` has no such field, and the chat context
//! strips the log from the draft it returns. The name scan below is the TRIPWIRE behind that: it
//! cannot prove a leak is impossible, only catch the obvious one if the structure is ever
//! bypassed (a new field, a new export path, a draft read where it should not be).
//!
//! This crate holds only the NAMES, as a deny-list. The shapes live in `camerata-ui-core`
//! (`review_timer`, `review_stats`), where a test pins that every key those shapes serialize is
//! listed here. The server's release-gate tests then inspect every client artifact for these
//! names. The two halves together are the enforcement of "internal only": a field added to the
//! internal shapes without being listed fails the first test, and a listed field that reaches a
//! client artifact fails the second.
//!
//! The names are deliberately distinctive (`review_*`, `reviewed_*`, `*_active_ms`) so a scan for
//! them has no false positives against ordinary export content.

/// The key under which the review log is saved in the onboarding draft.
pub const REVIEW_LOG_KEY: &str = "review_log";

/// Every JSON key the internal review-time shapes serialize (the draft's log and the stats JSON),
/// except the few generic words in [`GENERIC_KEYS`].
pub const INTERNAL_ONLY_KEYS: &[&str] = &[
    // The draft's log.
    REVIEW_LOG_KEY,
    "review_groups",
    "review_rows",
    "opened_at_ms",
    "active_ms",
    "group_reviewed",
    "reviewed_by",
    "reviewed_at_ms",
    "review_decision_ms",
    "review_samples",
    "review_unreadable",
    // The stats JSON.
    "review_stats",
    "total_active_ms",
    "total_active_hours",
    "unattributed_ms",
    "by_group_type",
    "rows_reviewed",
    "groups_reviewed",
    "active_hours",
];

/// Words the internal shapes also use that are ordinary elsewhere, so they are NOT searched for in
/// client artifacts. The key-coverage test in `camerata-ui-core` accepts them alongside
/// [`INTERNAL_ONLY_KEYS`].
pub const GENERIC_KEYS: &[&str] = &["label", "groups", "by_repo", "by_severity"];

/// Phrases the internal stats view shows. A client artifact must not carry them either.
pub const INTERNAL_ONLY_PHRASES: &[&str] = &["Active review time", "Review timer", "review timer"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deny_list_has_no_duplicates_and_no_generic_overlap() {
        let mut seen = std::collections::BTreeSet::new();
        for key in INTERNAL_ONLY_KEYS {
            assert!(seen.insert(*key), "duplicate {key}");
            assert!(
                !GENERIC_KEYS.contains(key),
                "{key} is both internal and generic"
            );
        }
    }

    #[test]
    fn the_names_stay_distinctive() {
        // A searched name that is a common word would flag ordinary export content.
        for key in INTERNAL_ONLY_KEYS {
            let distinctive = [
                "review",
                "active_",
                "_active",
                "opened_at",
                "unattributed",
                "by_group_type",
            ]
            .iter()
            .any(|marker| key.contains(marker));
            assert!(
                distinctive,
                "{key} is not distinctive enough to scan client artifacts for"
            );
        }
    }
}
