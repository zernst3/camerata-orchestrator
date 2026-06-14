//! `maintenance-demo` -- Tier-2 standing ops agent over a published app.
//!
//! Demonstrates the full lifecycle the maintenance agent runs on a schedule
//! after an app has been published: scan, security recommendation, approval
//! gate, and key-rotation schedule. Nothing changes on the live app without
//! an explicit approval that then runs through the SAME governed build+QA loop
//! as any feature change.
//!
//! Sections:
//!
//!   1. SCAN               -- run StubScanner; print each finding
//!   2. SECURITY REC       -- call security_warning; print the calm copy
//!   3. APPROVAL GATE      -- approve security findings, defer the rest
//!   4. KEY ROTATION       -- show which credentials are due for rotation
//!   5. SUMMARY            -- MAINTENANCE-DEMO: PASS

use camerata_maintenance::{
    due_rotations, security_warning, ApprovalDecision, KeyRotation, MaintenancePlan,
    MaintenanceScanner, StubScanner,
};
use chrono::{TimeZone, Utc};

// ── helpers ───────────────────────────────────────────────────────────────────

/// A fixed timestamp used throughout the demo so output is deterministic.
pub fn demo_now() -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap()
}

// ── main demo entry-point ─────────────────────────────────────────────────────

/// Run the full Tier-2 standing ops agent demonstration in-process.
pub async fn run_maintenance_demo() -> anyhow::Result<()> {
    println!("== Camerata MAINTENANCE-DEMO: Tier-2 standing ops agent ==");
    println!();

    // ── 1. SCAN ───────────────────────────────────────────────────────────────
    println!("── 1. SCAN ──");

    let scanner = StubScanner::new();
    let scan = scanner.scan("demo-app", demo_now()).await?;

    println!("  app:     {}", scan.app_id);
    println!(
        "  scanned: {}",
        scan.scanned_at.format("%Y-%m-%dT%H:%M:%SZ")
    );
    println!("  findings: {}", scan.findings.len());
    println!();

    for finding in &scan.findings {
        println!(
            "  [{:?}] {:?}: {}",
            finding.severity, finding.kind, finding.summary
        );
        println!("    detail: {}", finding.detail);
        println!("    recommendation: {}", finding.recommendation);
        println!();
    }

    // ── 2. SECURITY RECOMMENDATION ────────────────────────────────────────────
    println!("── 2. SECURITY RECOMMENDATION ──");

    match security_warning(&scan) {
        Some(msg) => {
            println!("  security recommendation (calm, plain-language):");
            println!("  \"{}\"", msg);
            println!("  NOTE: no CVE ids, no package names, no alarming language.");
        }
        None => {
            println!("  no security findings in this scan -- no recommendation emitted.");
        }
    }
    println!();

    // ── 3. APPROVAL GATE ──────────────────────────────────────────────────────
    println!("── 3. APPROVAL GATE ──");
    println!("  The user reviews findings and approves or defers each one.");
    println!("  Security findings: approved. All others: deferred for now.");
    println!();

    // Collect approval decisions: approve security findings, defer everything else.
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

    let plan = MaintenancePlan::from_decisions(&scan, &decisions);

    println!("  approved findings ({}):", plan.approved.len());
    for f in &plan.approved {
        println!("    [{}] {} -- {}", f.id, f.summary, f.recommendation);
    }

    let deferred_count = scan.findings.len() - plan.approved.len();
    println!("  deferred findings: {deferred_count} (surface again on next scan)");
    println!();
    println!("  GATE CONTRACT:");
    println!("    Nothing applies to the live app without an explicit approval.");
    println!(
        "    Approved items run through the SAME governed build+QA loop as any feature change."
    );
    println!("    The user, not the agent, decides what changes and when.");
    println!();

    // ── 4. KEY ROTATION ───────────────────────────────────────────────────────
    println!("── 4. KEY ROTATION ──");
    println!("  Rotation schedule for demo-app credentials:");
    println!();

    // Fixed baseline: keys were last rotated on 2026-01-01.
    let last_rotated = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

    let keys = vec![
        // 30-day interval, 164 days since Jan 1 -- due.
        KeyRotation::new("stripe-api-key", last_rotated, 30),
        // 90-day interval, 164 days since Jan 1 -- due.
        KeyRotation::new("db-password", last_rotated, 90),
        // 365-day interval, 164 days since Jan 1 -- not due.
        KeyRotation::new("jwt-signing-secret", last_rotated, 365),
    ];

    for key in &keys {
        let due = key.is_due(demo_now());
        let status = if due { "DUE" } else { "ok" };
        println!(
            "  [{status}] {} (interval: {} days, last rotated: {})",
            key.key_id,
            key.interval_days,
            key.last_rotated.format("%Y-%m-%d")
        );
    }
    println!();

    let due = due_rotations(&keys, demo_now());
    println!("  due for rotation: {}", due.len());
    for key in &due {
        println!("    {} -- schedule a rotation soon", key.key_id);
    }
    println!();

    // ── 5. SUMMARY ────────────────────────────────────────────────────────────
    println!("── SUMMARY ──");
    println!(
        "  Scan ran against demo-app with {} finding(s).",
        scan.findings.len()
    );
    println!("  Security recommendation emitted (calm, plain-language, no alarming copy).");
    println!(
        "  Approval gate: {} approved, {} deferred.",
        plan.approved.len(),
        deferred_count
    );
    println!("  Nothing applies to the live app without user approval + governed build.");
    println!(
        "  Key rotation schedule checked: {} credential(s) due.",
        due.len()
    );
    println!();
    println!("MAINTENANCE-DEMO: PASS");

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_maintenance::{
        ApprovalDecision, MaintenanceFinding, MaintenanceKind, MaintenancePlan, MaintenanceScan,
        Severity,
    };

    // ── plan-from-decisions: approve only security ────────────────────────────

    #[test]
    fn plan_from_decisions_approves_only_security_findings() {
        let now = demo_now();
        let scan = MaintenanceScan::new(
            "demo-app",
            now,
            vec![
                MaintenanceFinding::new(
                    "f-dep",
                    MaintenanceKind::DependencyUpgrade,
                    Severity::Recommended,
                    "A newer library is available.",
                    "Details.",
                    "Upgrade when convenient.",
                ),
                MaintenanceFinding::new(
                    "f-sec",
                    MaintenanceKind::SecurityPatch,
                    Severity::Critical,
                    "A security fix is available.",
                    "Details.",
                    "Apply the fix.",
                ),
                MaintenanceFinding::new(
                    "f-key",
                    MaintenanceKind::KeyRotation,
                    Severity::Important,
                    "A credential rotation is due.",
                    "Details.",
                    "Rotate the credential.",
                ),
            ],
        );

        let decisions: Vec<(String, ApprovalDecision)> = scan
            .findings
            .iter()
            .map(|f| {
                let d = if f.is_security() {
                    ApprovalDecision::Approve
                } else {
                    ApprovalDecision::Defer
                };
                (f.id.clone(), d)
            })
            .collect();

        let plan = MaintenancePlan::from_decisions(&scan, &decisions);

        // Only the security finding must be approved.
        assert_eq!(
            plan.approved.len(),
            1,
            "exactly one finding must be approved"
        );
        assert_eq!(plan.approved[0].id, "f-sec");
    }

    // ── deferred findings do not appear in the plan ───────────────────────────

    #[test]
    fn deferred_findings_are_not_in_the_plan() {
        let now = demo_now();
        let scan = MaintenanceScan::new(
            "demo-app",
            now,
            vec![MaintenanceFinding::new(
                "f-dep",
                MaintenanceKind::DependencyUpgrade,
                Severity::Recommended,
                "A newer library is available.",
                "Details.",
                "Upgrade when convenient.",
            )],
        );

        let decisions = vec![("f-dep".to_string(), ApprovalDecision::Defer)];
        let plan = MaintenancePlan::from_decisions(&scan, &decisions);

        assert!(plan.is_empty(), "no approved findings: plan must be empty");
    }

    // ── due_rotations returns the correct subset ──────────────────────────────

    #[test]
    fn due_rotations_returns_correct_subset() {
        use chrono::TimeZone;
        let last_rotated = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let now = demo_now(); // 2026-06-14 = ~164 days after Jan 1

        let keys = vec![
            KeyRotation::new("stripe-api-key", last_rotated, 30), // due
            KeyRotation::new("db-password", last_rotated, 90),    // due
            KeyRotation::new("jwt-signing-secret", last_rotated, 365), // not due
        ];

        let due = due_rotations(&keys, now);
        assert_eq!(due.len(), 2);
        assert!(due.iter().any(|k| k.key_id == "stripe-api-key"));
        assert!(due.iter().any(|k| k.key_id == "db-password"));
        assert!(!due.iter().any(|k| k.key_id == "jwt-signing-secret"));
    }

    // ── security_warning is calm ──────────────────────────────────────────────

    #[test]
    fn security_warning_copy_is_calm_and_present_when_security_found() {
        use camerata_maintenance::security_warning;
        let now = demo_now();
        let scan = MaintenanceScan::new(
            "demo-app",
            now,
            vec![MaintenanceFinding::new(
                "sec-1",
                MaintenanceKind::SecurityPatch,
                Severity::Critical,
                "A security fix is available for part of your app.",
                "Details.",
                "Bring your app up to date.",
            )],
        );

        let warning = security_warning(&scan);
        assert!(
            warning.is_some(),
            "warning must be present when security finding exists"
        );
        let msg = warning.unwrap();
        assert!(!msg.contains('!'), "copy must not use exclamation marks");
        assert!(
            msg.to_lowercase().contains("security fix"),
            "copy must mention 'security fix'"
        );
    }

    // ── full demo must complete without error ─────────────────────────────────

    #[tokio::test]
    async fn maintenance_demo_runs_without_error() {
        run_maintenance_demo()
            .await
            .expect("maintenance-demo must not error");
    }
}
