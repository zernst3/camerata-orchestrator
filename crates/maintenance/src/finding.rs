//! Core finding types: the unit of work the maintenance agent surfaces.
//!
//! A [`MaintenanceFinding`] is one discrete ops item the agent found (a
//! dependency upgrade, a security patch, a key that is due for rotation, etc.).
//! Findings are the raw output of a scan; the user never applies them
//! directly. They flow through [`crate::plan::MaintenancePlan`] via an
//! explicit approval step before anything runs.

use serde::{Deserialize, Serialize};

// ─── kind ────────────────────────────────────────────────────────────────────

/// The category of a maintenance item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaintenanceKind {
    /// A newer version of a library or runtime the app depends on is available.
    DependencyUpgrade,
    /// A known security vulnerability has a fix available for a dependency.
    SecurityPatch,
    /// A key, secret, or API credential is due for rotation.
    KeyRotation,
    /// A TLS certificate or similar is approaching expiry.
    CertRenewal,
    /// A backup is due or the most recent backup is missing.
    Backup,
    /// A health probe or operational hygiene item needs attention.
    HealthCheck,
}

// ─── severity ────────────────────────────────────────────────────────────────

/// How urgently a finding should be acted on.
///
/// Ordered from least to most urgent: `Info < Recommended < Important < Critical`.
/// `Critical` is reserved for security vulnerabilities.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational only; no action required.
    Info,
    /// Worthwhile to address at the next opportunity.
    Recommended,
    /// Should be addressed soon; things may degrade if left unattended.
    Important,
    /// Requires prompt attention; reserved for security vulnerabilities.
    Critical,
}

// ─── finding ─────────────────────────────────────────────────────────────────

/// A single maintenance item surfaced by a scan.
///
/// `summary` and `recommendation` are plain-language consumer copy: no package
/// names, no version numbers, no technical jargon. The goal is "a regular person
/// can read this and understand what to do."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceFinding {
    /// A stable identifier for this finding within a scan. Used as the key
    /// in approval decisions (see [`crate::plan::MaintenancePlan::from_decisions`]).
    pub id: String,
    /// The category of maintenance item.
    pub kind: MaintenanceKind,
    /// How urgently this finding should be acted on.
    pub severity: Severity,
    /// One sentence, plain language, describing what was found.
    pub summary: String,
    /// More detail about the finding (still plain language, slightly longer).
    pub detail: String,
    /// What the user should do, written as a calm recommendation.
    pub recommendation: String,
}

impl MaintenanceFinding {
    /// Construct a finding.
    pub fn new(
        id: impl Into<String>,
        kind: MaintenanceKind,
        severity: Severity,
        summary: impl Into<String>,
        detail: impl Into<String>,
        recommendation: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            kind,
            severity,
            summary: summary.into(),
            detail: detail.into(),
            recommendation: recommendation.into(),
        }
    }

    /// Returns `true` if this finding is security-related: either a
    /// [`MaintenanceKind::SecurityPatch`] or [`Severity::Critical`] severity.
    /// Both conditions independently trigger the security path, because a
    /// critical key-rotation or cert-renewal item is just as urgent as a CVE.
    pub fn is_security(&self) -> bool {
        self.kind == MaintenanceKind::SecurityPatch || self.severity == Severity::Critical
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info_finding() -> MaintenanceFinding {
        MaintenanceFinding::new(
            "f-dep-1",
            MaintenanceKind::DependencyUpgrade,
            Severity::Recommended,
            "A newer version of a library your app uses is available.",
            "One of the libraries your app is built on has a newer release.",
            "Bring your app up to date when it is convenient.",
        )
    }

    fn security_finding() -> MaintenanceFinding {
        MaintenanceFinding::new(
            "f-sec-1",
            MaintenanceKind::SecurityPatch,
            Severity::Critical,
            "A security fix is available for part of your app.",
            "A vulnerability was found and patched in a component your app uses.",
            "It is a good idea to bring your app up to date. A part of it has a security fix available.",
        )
    }

    fn critical_non_security_finding() -> MaintenanceFinding {
        MaintenanceFinding::new(
            "f-cert-1",
            MaintenanceKind::CertRenewal,
            Severity::Critical,
            "Your app certificate is about to expire.",
            "The certificate your app uses will expire soon.",
            "Renew your certificate to keep the app accessible.",
        )
    }

    #[test]
    fn severity_ordering() {
        assert!(Severity::Info < Severity::Recommended);
        assert!(Severity::Recommended < Severity::Important);
        assert!(Severity::Important < Severity::Critical);
        assert!(Severity::Critical > Severity::Info);
    }

    #[test]
    fn is_security_true_for_security_patch_kind() {
        let f = security_finding();
        assert!(f.is_security());
    }

    #[test]
    fn is_security_true_for_critical_severity_regardless_of_kind() {
        let f = critical_non_security_finding();
        // CertRenewal is not a SecurityPatch, but Critical triggers the security path.
        assert_eq!(f.kind, MaintenanceKind::CertRenewal);
        assert!(f.is_security());
    }

    #[test]
    fn is_security_false_for_non_critical_non_security() {
        let f = info_finding();
        assert!(!f.is_security());
    }

    #[test]
    fn finding_round_trip_json() {
        let f = security_finding();
        let json = serde_json::to_string(&f).unwrap();
        let back: MaintenanceFinding = serde_json::from_str(&json).unwrap();
        assert_eq!(back, f);
    }

    #[test]
    fn severity_round_trip_json() {
        for s in [
            Severity::Info,
            Severity::Recommended,
            Severity::Important,
            Severity::Critical,
        ] {
            let json = serde_json::to_string(&s).unwrap();
            let back: Severity = serde_json::from_str(&json).unwrap();
            assert_eq!(back, s);
        }
    }

    #[test]
    fn kind_round_trip_json() {
        for k in [
            MaintenanceKind::DependencyUpgrade,
            MaintenanceKind::SecurityPatch,
            MaintenanceKind::KeyRotation,
            MaintenanceKind::CertRenewal,
            MaintenanceKind::Backup,
            MaintenanceKind::HealthCheck,
        ] {
            let json = serde_json::to_string(&k).unwrap();
            let back: MaintenanceKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, k);
        }
    }
}
