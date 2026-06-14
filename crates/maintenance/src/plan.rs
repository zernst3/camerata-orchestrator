//! The governed-update approval gate.
//!
//! Nothing in a scan is ever applied to a live app without the user's
//! explicit approval. [`MaintenancePlan`] is the approved subset of a scan:
//! only findings the user `Approve`d enter it. `Defer` and `Dismiss` are both
//! "not this time" decisions and are not included.
//!
//! The approved plan's items run through the SAME governed build-and-QA loop
//! as any feature change. This crate models the APPROVAL GATE, not the
//! execution: once a plan is approved it is handed off to the fleet coordinator
//! exactly the way a user story is.

use serde::{Deserialize, Serialize};

use crate::finding::MaintenanceFinding;
use crate::scan::MaintenanceScan;

// ─── approval decision ────────────────────────────────────────────────────────

/// The user's decision for a single maintenance finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Apply this finding through the governed update loop.
    Approve,
    /// Skip for now; surface again on the next scan.
    Defer,
    /// Dismiss; do not surface again (the agent may re-surface if the
    /// underlying condition persists or worsens).
    Dismiss,
}

// ─── maintenance plan ────────────────────────────────────────────────────────

/// A set of findings the user approved for application.
///
/// Built via [`MaintenancePlan::from_decisions`] from a scan + per-finding
/// approval decisions. Only `Approve`d findings enter the plan.
///
/// The approved plan's items run through the SAME governed build-and-QA loop
/// as a feature change. Nothing about the app is changed outside that gate.
/// This struct models the approval gate; execution is the fleet coordinator's
/// responsibility.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenancePlan {
    /// The identifier of the app the plan is for.
    pub app_id: String,
    /// The findings the user approved. Only approved findings are included.
    pub approved: Vec<MaintenanceFinding>,
}

impl MaintenancePlan {
    /// Build an approved plan from a scan and a list of per-finding decisions.
    ///
    /// `decisions` is a slice of `(finding_id, ApprovalDecision)` pairs. Only
    /// findings whose id appears with [`ApprovalDecision::Approve`] enter the
    /// plan. Findings with no entry in `decisions` default to
    /// [`ApprovalDecision::Defer`] (not applied). Unknown ids in `decisions`
    /// (ids not present in the scan) are silently ignored.
    pub fn from_decisions(
        scan: &MaintenanceScan,
        decisions: &[(String, ApprovalDecision)],
    ) -> MaintenancePlan {
        let approved: Vec<MaintenanceFinding> = scan
            .findings
            .iter()
            .filter(|f| {
                decisions
                    .iter()
                    .any(|(id, dec)| id == &f.id && *dec == ApprovalDecision::Approve)
            })
            .cloned()
            .collect();

        MaintenancePlan {
            app_id: scan.app_id.clone(),
            approved,
        }
    }

    /// Returns `true` if no findings were approved (nothing to apply).
    pub fn is_empty(&self) -> bool {
        self.approved.is_empty()
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

    fn three_finding_scan() -> MaintenanceScan {
        MaintenanceScan::new(
            "app-plan",
            fixed_now(),
            vec![
                MaintenanceFinding::new(
                    "f-dep",
                    MaintenanceKind::DependencyUpgrade,
                    Severity::Recommended,
                    "Dependency upgrade available.",
                    "Details.",
                    "Upgrade when convenient.",
                ),
                MaintenanceFinding::new(
                    "f-sec",
                    MaintenanceKind::SecurityPatch,
                    Severity::Critical,
                    "Security fix available.",
                    "Details.",
                    "Apply the fix.",
                ),
                MaintenanceFinding::new(
                    "f-key",
                    MaintenanceKind::KeyRotation,
                    Severity::Important,
                    "Credential rotation due.",
                    "Details.",
                    "Rotate the credential.",
                ),
            ],
        )
    }

    #[test]
    fn from_decisions_includes_only_approved() {
        let scan = three_finding_scan();
        let decisions = vec![
            ("f-dep".to_string(), ApprovalDecision::Approve),
            ("f-sec".to_string(), ApprovalDecision::Defer),
            ("f-key".to_string(), ApprovalDecision::Dismiss),
        ];
        let plan = MaintenancePlan::from_decisions(&scan, &decisions);
        assert_eq!(plan.approved.len(), 1);
        assert_eq!(plan.approved[0].id, "f-dep");
    }

    #[test]
    fn from_decisions_approve_multiple() {
        let scan = three_finding_scan();
        let decisions = vec![
            ("f-dep".to_string(), ApprovalDecision::Approve),
            ("f-sec".to_string(), ApprovalDecision::Approve),
            ("f-key".to_string(), ApprovalDecision::Defer),
        ];
        let plan = MaintenancePlan::from_decisions(&scan, &decisions);
        assert_eq!(plan.approved.len(), 2);
        assert!(plan.approved.iter().any(|f| f.id == "f-dep"));
        assert!(plan.approved.iter().any(|f| f.id == "f-sec"));
    }

    #[test]
    fn from_decisions_absent_id_defaults_to_defer_not_applied() {
        let scan = three_finding_scan();
        // Only decide on one; the others have no entry and must not appear.
        let decisions = vec![("f-sec".to_string(), ApprovalDecision::Approve)];
        let plan = MaintenancePlan::from_decisions(&scan, &decisions);
        assert_eq!(plan.approved.len(), 1);
        assert_eq!(plan.approved[0].id, "f-sec");
    }

    #[test]
    fn from_decisions_all_deferred_gives_empty_plan() {
        let scan = three_finding_scan();
        let decisions = vec![
            ("f-dep".to_string(), ApprovalDecision::Defer),
            ("f-sec".to_string(), ApprovalDecision::Defer),
            ("f-key".to_string(), ApprovalDecision::Defer),
        ];
        let plan = MaintenancePlan::from_decisions(&scan, &decisions);
        assert!(plan.is_empty());
    }

    #[test]
    fn from_decisions_unknown_ids_silently_ignored() {
        let scan = three_finding_scan();
        // Include decisions for ids that do not exist in the scan.
        let decisions = vec![
            ("f-dep".to_string(), ApprovalDecision::Approve),
            ("f-unknown".to_string(), ApprovalDecision::Approve),
        ];
        let plan = MaintenancePlan::from_decisions(&scan, &decisions);
        assert_eq!(plan.approved.len(), 1);
        assert_eq!(plan.approved[0].id, "f-dep");
    }

    #[test]
    fn is_empty_true_for_no_approved_findings() {
        let plan = MaintenancePlan {
            app_id: "app-1".to_string(),
            approved: vec![],
        };
        assert!(plan.is_empty());
    }

    #[test]
    fn is_empty_false_when_approved_findings_present() {
        let scan = three_finding_scan();
        let decisions = vec![("f-dep".to_string(), ApprovalDecision::Approve)];
        let plan = MaintenancePlan::from_decisions(&scan, &decisions);
        assert!(!plan.is_empty());
    }

    #[test]
    fn plan_round_trip_json() {
        let scan = three_finding_scan();
        let decisions = vec![("f-sec".to_string(), ApprovalDecision::Approve)];
        let plan = MaintenancePlan::from_decisions(&scan, &decisions);
        let json = serde_json::to_string(&plan).unwrap();
        let back: MaintenancePlan = serde_json::from_str(&json).unwrap();
        assert_eq!(back, plan);
    }

    #[test]
    fn approval_decision_round_trip_json() {
        for d in [
            ApprovalDecision::Approve,
            ApprovalDecision::Defer,
            ApprovalDecision::Dismiss,
        ] {
            let json = serde_json::to_string(&d).unwrap();
            let back: ApprovalDecision = serde_json::from_str(&json).unwrap();
            assert_eq!(back, d);
        }
    }
}
