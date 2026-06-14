//! User-facing security warning copy.
//!
//! [`security_warning`] returns a calm, plain-language message when a scan
//! contains a security finding, and `None` otherwise. The copy is never
//! alarming: no exclamation marks, no all-caps severity labels, no technical
//! vulnerability IDs.

use crate::scan::MaintenanceScan;

/// Returns a calm plain-language recommendation when the scan contains a
/// security finding, or `None` when there is nothing security-related.
///
/// The message is suitable to show directly to a non-technical user. It never
/// mentions CVE ids, package names, or version numbers. It never uses alarming
/// language.
pub fn security_warning(scan: &MaintenanceScan) -> Option<String> {
    if scan.has_security() {
        Some(
            "It is a good idea to bring your app up to date. \
             A part of it has a security fix available."
                .to_string(),
        )
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::finding::{MaintenanceFinding, MaintenanceKind, Severity};
    use crate::scan::MaintenanceScan;
    use chrono::{TimeZone, Utc};

    fn fixed_now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap()
    }

    fn scan_with_security() -> MaintenanceScan {
        MaintenanceScan::new(
            "app-sec",
            fixed_now(),
            vec![MaintenanceFinding::new(
                "s1",
                MaintenanceKind::SecurityPatch,
                Severity::Critical,
                "A security fix is available.",
                "A vulnerability was patched.",
                "Bring your app up to date.",
            )],
        )
    }

    fn scan_without_security() -> MaintenanceScan {
        MaintenanceScan::new(
            "app-nosec",
            fixed_now(),
            vec![MaintenanceFinding::new(
                "d1",
                MaintenanceKind::DependencyUpgrade,
                Severity::Recommended,
                "A newer library version is available.",
                "A library update is available.",
                "Update when convenient.",
            )],
        )
    }

    #[test]
    fn warning_is_some_when_security_finding_present() {
        let w = security_warning(&scan_with_security());
        assert!(w.is_some(), "expected Some when scan has security findings");
    }

    #[test]
    fn warning_is_none_when_no_security_finding() {
        let w = security_warning(&scan_without_security());
        assert!(
            w.is_none(),
            "expected None when scan has no security findings"
        );
    }

    #[test]
    fn warning_copy_is_calm_no_alarming_words() {
        let w = security_warning(&scan_with_security()).unwrap();
        let lower = w.to_lowercase();
        // No alarming language.
        assert!(!lower.contains("urgent"), "copy must not say urgent");
        assert!(!lower.contains("danger"), "copy must not say danger");
        assert!(!w.contains('!'), "copy must not use exclamation marks");
        // No all-caps words (each word should be lowercase or title-case for "I").
        for word in w.split_whitespace() {
            let stripped = word.trim_matches(|c: char| !c.is_alphabetic());
            if stripped.len() > 1 {
                assert!(
                    !stripped.chars().all(|c| c.is_uppercase()),
                    "copy must not contain all-caps word: {word}"
                );
            }
        }
    }

    #[test]
    fn warning_copy_contains_expected_calm_phrasing() {
        let w = security_warning(&scan_with_security()).unwrap();
        assert!(
            w.contains("good idea"),
            "copy should include 'good idea' phrasing"
        );
        assert!(
            w.contains("security fix"),
            "copy should mention 'security fix'"
        );
    }

    #[test]
    fn warning_is_none_for_empty_scan() {
        let scan = MaintenanceScan::new("app-empty", fixed_now(), vec![]);
        assert!(security_warning(&scan).is_none());
    }
}
