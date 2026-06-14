//! Bridge from the Live screen to the maintenance ops agent (`camerata-maintenance`).
//!
//! Thin and testable: each public function delegates to a single crate-level
//! primitive, keeping the Dioxus layer free of any domain logic.
//!
//! In the prototype the scanner is always `StubScanner` (no network calls).
//! A production path swaps in a real registry-backed implementation behind the
//! same `MaintenanceScanner` trait.

use camerata_maintenance::{
    ApprovalDecision, MaintenancePlan, MaintenanceScan, MaintenanceScanner, StubScanner,
};
use chrono::Utc;

/// Run a maintenance scan for `app_id` and return the result.
///
/// Uses `StubScanner` (the deterministic offline scanner) so the prototype
/// works with no network access. `scanned_at` is set to the current wall-clock
/// time: this is the live app path so `Utc::now()` is appropriate here. Unit
/// tests that need a fixed timestamp should call the scanner directly via the
/// helpers in the `#[cfg(test)]` block below.
pub async fn scan_app(app_id: &str) -> MaintenanceScan {
    let scanner = StubScanner::new();
    // The trait is infallible for StubScanner; unwrap is safe.
    scanner
        .scan(app_id, Utc::now())
        .await
        .unwrap_or_else(|_| MaintenanceScan::new(app_id, Utc::now(), vec![]))
}

/// Returns a calm, plain-language security recommendation when the scan
/// contains a security finding, or `None` when there is nothing to flag.
///
/// Delegates directly to `camerata_maintenance::security_warning`.
pub fn warning_for(scan: &MaintenanceScan) -> Option<String> {
    camerata_maintenance::security_warning(scan)
}

/// Build a `MaintenancePlan` that approves every security finding in the scan
/// and defers the rest. Models "the user clicked Update now on the security
/// recommendation."
///
/// In a real deployment the returned plan is handed to the governed build-and-QA
/// loop exactly the way a feature change is. That wiring is out of scope for the
/// prototype UI; the comment below notes the handoff point.
pub fn approve_all_security(scan: &MaintenanceScan) -> MaintenancePlan {
    let decisions: Vec<(String, ApprovalDecision)> = scan
        .findings
        .iter()
        .map(|f| {
            let decision = if f.is_security() {
                ApprovalDecision::Approve
            } else {
                ApprovalDecision::Defer
            };
            (f.id.clone(), decision)
        })
        .collect();

    // TODO(real deployment): hand the returned plan to the fleet coordinator
    // so the approved updates run through the same governed build-and-QA loop
    // as a feature change. Nothing in the app changes outside that gate.
    MaintenancePlan::from_decisions(scan, &decisions)
}

// ─── tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_maintenance::{
        MaintenanceFinding, MaintenanceKind, MaintenanceScan, MaintenanceScanner, Severity,
        StubScanner,
    };
    use chrono::{TimeZone, Utc};

    fn fixed_now() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap()
    }

    /// A deterministic substitute for `scan_app` that uses a fixed timestamp,
    /// making tests reproducible regardless of when they run.
    async fn scan_app_fixed(app_id: &str) -> MaintenanceScan {
        StubScanner::new().scan(app_id, fixed_now()).await.unwrap()
    }

    // --- scan_app equivalent: default stub returns the expected 3-finding set ---

    #[tokio::test]
    async fn scan_returns_the_default_stub_findings() {
        let scan = scan_app_fixed("my-app").await;
        assert_eq!(scan.app_id, "my-app");
        assert_eq!(scan.findings.len(), 3);
        assert!(scan
            .findings
            .iter()
            .any(|f| f.kind == MaintenanceKind::DependencyUpgrade));
        assert!(scan
            .findings
            .iter()
            .any(|f| f.kind == MaintenanceKind::SecurityPatch));
        assert!(scan
            .findings
            .iter()
            .any(|f| f.kind == MaintenanceKind::KeyRotation));
    }

    // --- warning_for: returns Some when a security finding is present ----------

    #[test]
    fn warning_for_returns_some_when_security_finding_present() {
        let scan = MaintenanceScan::new(
            "app-1",
            fixed_now(),
            vec![MaintenanceFinding::new(
                "sec-1",
                MaintenanceKind::SecurityPatch,
                Severity::Critical,
                "A security fix is available.",
                "A vulnerability was found and patched.",
                "Bring your app up to date.",
            )],
        );
        assert!(warning_for(&scan).is_some());
    }

    #[test]
    fn warning_for_returns_none_when_no_security_finding() {
        let scan = MaintenanceScan::new(
            "app-2",
            fixed_now(),
            vec![MaintenanceFinding::new(
                "dep-1",
                MaintenanceKind::DependencyUpgrade,
                Severity::Recommended,
                "A newer library version is available.",
                "One of your app dependencies has a new release.",
                "Update when convenient.",
            )],
        );
        assert!(warning_for(&scan).is_none());
    }

    // --- approve_all_security: plan contains security findings, not others -----

    #[tokio::test]
    async fn approve_all_security_includes_security_findings_only() {
        let scan = scan_app_fixed("app-3").await;
        // Default stub: SecurityPatch (Critical) + DependencyUpgrade (Recommended)
        // + KeyRotation (Important). Only the SecurityPatch is_security().
        let plan = approve_all_security(&scan);
        assert_eq!(plan.app_id, "app-3");

        // Every approved finding must be security-related.
        for f in &plan.approved {
            assert!(
                f.is_security(),
                "non-security finding should not be approved: {}",
                f.id
            );
        }

        // The security finding must be approved.
        assert!(
            plan.approved
                .iter()
                .any(|f| f.kind == MaintenanceKind::SecurityPatch),
            "expected SecurityPatch finding to be approved"
        );

        // Non-security findings must NOT be in the plan.
        assert!(
            !plan
                .approved
                .iter()
                .any(|f| f.kind == MaintenanceKind::DependencyUpgrade),
            "DependencyUpgrade should be deferred, not approved"
        );
        assert!(
            !plan
                .approved
                .iter()
                .any(|f| f.kind == MaintenanceKind::KeyRotation && !f.is_security()),
            "non-security KeyRotation should be deferred, not approved"
        );
    }

    #[test]
    fn approve_all_security_on_empty_scan_gives_empty_plan() {
        let scan = MaintenanceScan::new("app-empty", fixed_now(), vec![]);
        let plan = approve_all_security(&scan);
        assert!(plan.is_empty());
    }
}
