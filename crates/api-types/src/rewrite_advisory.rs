//! The names of the OPERATOR-ONLY rewrite advisory: the internal note that tells the architect
//! whether a run's numbers are worth a fix-or-rewrite talk, with the numbers behind it.
//!
//! Zach's ruling: whether a client fixes or rewrites is the architect's decision alone. No client
//! file ever states or hints at it. The engine gives the operator an internal note
//! (`rewrite_advisory.json` and `.md` in the run's state directory, and a panel in the cockpit)
//! and nothing else.
//!
//! WHICH GUARD IS WHICH. The primary guard is structural: the advisory's type lives outside
//! `AuditReportJson` and `ProductPackage`, and the export request carries no advisory field (a
//! test pins the request body's keys). The names below are the TRIPWIRE behind that: the release
//! gate refuses any package, client or internal, that carries one of them in a zip member, a
//! workbook part or cell, the PDF's text or a file name. A tripwire cannot prove a leak is
//! impossible; it catches the obvious one if the structure is ever bypassed.
//!
//! This crate holds only the NAMES. The server's advisory and the `camerata-ui-core` shape each
//! pin that every key they serialize is either listed in [`INTERNAL_ONLY_KEYS`] or is one of the
//! ordinary words in [`GENERIC_KEYS`], which are not searched for.
//!
//! The bare word "rewrite" is NOT on the list (owner ruling): a client's own code and an ordinary
//! fix can say it. Only the distinctive keys and phrases are.

/// The advisory's distinctive JSON keys. The gate searches every client surface for these.
pub const INTERNAL_ONLY_KEYS: &[&str] = &[
    "rewrite_advisory",
    "rewrite_signals",
    "fix_hours_per_kloc",
    "structural_share",
    "unsettled_areas",
    "repeat_cause_share",
];

/// Phrases the advisory's text uses. The gate searches every client surface for these, ignoring
/// case.
pub const INTERNAL_ONLY_PHRASES: &[&str] = &[
    "Rewrite advisory",
    "rewrite conversation",
    "worth a conversation",
];

/// Ordinary words the advisory's shapes also use as keys. They are common in client files too, so
/// they are NOT searched for; the key-coverage tests accept them alongside
/// [`INTERNAL_ONLY_KEYS`].
pub const GENERIC_KEYS: &[&str] = &[
    // The document.
    "run_id",
    "finished_at",
    "outcome",
    "signals_over",
    "code_lines",
    "files_not_reviewed",
    "lines",
    // Why there is not enough to say.
    "guards",
    "reason",
    "detail",
    // The Fixes counted.
    "fixes",
    "counted",
    "without_estimate",
    // Every signal.
    "over",
    "threshold",
    "percent",
    // Fix hours and the structural share.
    "low_hours",
    "high_hours",
    "low",
    "high",
    "structural_low_hours",
    "structural_high_hours",
    // Areas without a settled habit.
    "assessed",
    "unsettled",
    "areas",
    "category",
    "assessed_rules",
    "unsettled_rules",
    "threshold_rules",
    // One cause repeated widely.
    "total_places",
    "largest_high",
    "top_three",
    "fix",
    "places",
    "threshold_one",
    "threshold_three",
    // The context line.
    "context",
    "critical_high_fixes",
    "per_kloc",
    // The limits the guards use.
    "limits",
    "min_code_lines",
    "max_unestimated_percent",
    "signals_to_fire",
];

/// Every deny-listed term (keys, then phrases).
pub fn terms() -> impl Iterator<Item = &'static str> {
    INTERNAL_ONLY_KEYS
        .iter()
        .chain(INTERNAL_ONLY_PHRASES)
        .copied()
}

/// Every deny-listed term in `text`, ignoring case, in list order, each once.
pub fn terms_in(text: &str) -> Vec<&'static str> {
    let lower = text.to_lowercase();
    terms()
        .filter(|term| lower.contains(&term.to_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_deny_list_names_the_six_keys_and_three_phrases_of_the_design() {
        for key in [
            "rewrite_advisory",
            "rewrite_signals",
            "fix_hours_per_kloc",
            "structural_share",
            "unsettled_areas",
            "repeat_cause_share",
        ] {
            assert!(INTERNAL_ONLY_KEYS.contains(&key), "{key} is not listed");
        }
        for phrase in [
            "Rewrite advisory",
            "rewrite conversation",
            "worth a conversation",
        ] {
            assert!(
                INTERNAL_ONLY_PHRASES.contains(&phrase),
                "{phrase} is not listed"
            );
        }
    }

    #[test]
    fn the_deny_list_has_no_duplicates_and_no_generic_overlap() {
        let mut seen = std::collections::BTreeSet::new();
        for term in terms() {
            assert!(seen.insert(term.to_lowercase()), "duplicate {term}");
            assert!(
                !GENERIC_KEYS.contains(&term),
                "{term} is both internal and generic"
            );
        }
        assert!(!GENERIC_KEYS.is_empty(), "the shapes use ordinary keys too");
    }

    /// Owner ruling: the bare word is not banned, so a client's own code and an ordinary fix can
    /// say it.
    #[test]
    fn the_bare_word_rewrite_is_not_on_the_list() {
        assert!(terms_in("Rewrite the query with a bound parameter.").is_empty());
        assert!(terms_in("let rewrite = true;").is_empty());
        for term in terms() {
            assert!(!term.eq_ignore_ascii_case("rewrite"), "{term}");
        }
    }

    #[test]
    fn a_term_is_found_whatever_its_case() {
        assert_eq!(
            terms_in("WORTH A CONVERSATION"),
            vec!["worth a conversation"]
        );
        assert_eq!(
            terms_in("{\"Structural_Share\":1}"),
            vec!["structural_share"]
        );
        assert_eq!(terms_in("the rewrite advisory"), vec!["Rewrite advisory"]);
    }

    /// A searched key that is a common word would refuse ordinary client packages.
    #[test]
    fn the_keys_stay_distinctive() {
        for key in INTERNAL_ONLY_KEYS {
            assert!(
                key.contains('_') && key.len() >= 15,
                "{key} is not distinctive enough to scan client artifacts for"
            );
        }
    }
}
