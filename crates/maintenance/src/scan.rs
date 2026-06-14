//! Scan result types and the [`MaintenanceScanner`] async trait.
//!
//! A [`MaintenanceScan`] is the full result of one scan run for one published
//! app. It contains all [`crate::finding::MaintenanceFinding`]s the agent
//! found, plus metadata (which app, when the scan ran).
//!
//! The [`MaintenanceScanner`] trait is the seam: production implementations
//! call real dependency registries and CVE feeds; [`StubScanner`] is the
//! deterministic offline default used for tests and the prototype UI.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::finding::{MaintenanceFinding, MaintenanceKind, Severity};

// ─── scan result ─────────────────────────────────────────────────────────────

/// The result of one maintenance scan for a published app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MaintenanceScan {
    /// The identifier of the app that was scanned.
    pub app_id: String,
    /// When the scan ran (caller-supplied; never computed inside this crate).
    pub scanned_at: DateTime<Utc>,
    /// All findings the scan produced, in the order they were emitted.
    pub findings: Vec<MaintenanceFinding>,
}

impl MaintenanceScan {
    /// Construct a scan result.
    pub fn new(
        app_id: impl Into<String>,
        scanned_at: DateTime<Utc>,
        findings: Vec<MaintenanceFinding>,
    ) -> Self {
        Self {
            app_id: app_id.into(),
            scanned_at,
            findings,
        }
    }

    /// Returns `true` if any finding is security-related.
    pub fn has_security(&self) -> bool {
        self.findings.iter().any(|f| f.is_security())
    }

    /// The highest severity across all findings, or `None` if there are none.
    pub fn highest_severity(&self) -> Option<Severity> {
        self.findings.iter().map(|f| f.severity.clone()).max()
    }

    /// All findings that are security-related.
    pub fn security_findings(&self) -> Vec<&MaintenanceFinding> {
        self.findings.iter().filter(|f| f.is_security()).collect()
    }
}

// ─── scanner seam ────────────────────────────────────────────────────────────

/// The async seam for producing a scan of a published app.
///
/// Production implementations call real registries (crates.io, npm, cargo-audit,
/// CVE feeds). [`StubScanner`] is the deterministic offline default: it always
/// returns the same believable set of findings so the rest of the system can be
/// developed and tested without network access.
#[async_trait]
pub trait MaintenanceScanner: Send + Sync {
    /// Scan the given app and return all findings as of `now`.
    ///
    /// `now` is caller-supplied so tests can pass a fixed timestamp and the
    /// pure logic never calls the wall clock.
    async fn scan(&self, app_id: &str, now: DateTime<Utc>) -> anyhow::Result<MaintenanceScan>;
}

// ─── stub scanner ────────────────────────────────────────────────────────────

/// A deterministic offline [`MaintenanceScanner`] for tests and the prototype.
///
/// Returns a believable set of findings regardless of network state. The
/// builder methods let callers add or replace findings to test specific
/// scenarios.
#[derive(Debug, Clone)]
pub struct StubScanner {
    findings: Vec<MaintenanceFinding>,
}

impl StubScanner {
    /// Construct a stub with the default believable set:
    /// - One `DependencyUpgrade` at `Recommended` severity.
    /// - One `SecurityPatch` at `Critical` severity.
    /// - One `KeyRotation` at `Important` severity.
    pub fn new() -> Self {
        Self {
            findings: default_stub_findings(),
        }
    }

    /// Replace the findings with a custom list. Builder form.
    pub fn with_findings(mut self, findings: Vec<MaintenanceFinding>) -> Self {
        self.findings = findings;
        self
    }

    /// Add one finding to the existing list. Builder form.
    pub fn push_finding(mut self, finding: MaintenanceFinding) -> Self {
        self.findings.push(finding);
        self
    }

    /// Clear all findings (useful for testing the empty-scan path). Builder form.
    pub fn empty(mut self) -> Self {
        self.findings.clear();
        self
    }
}

impl Default for StubScanner {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MaintenanceScanner for StubScanner {
    async fn scan(&self, app_id: &str, now: DateTime<Utc>) -> anyhow::Result<MaintenanceScan> {
        Ok(MaintenanceScan::new(app_id, now, self.findings.clone()))
    }
}

/// The canonical default set of believable stub findings.
fn default_stub_findings() -> Vec<MaintenanceFinding> {
    vec![
        MaintenanceFinding::new(
            "stub-dep-upgrade-1",
            MaintenanceKind::DependencyUpgrade,
            Severity::Recommended,
            "A newer version of a library your app uses is available.",
            "One of the building blocks your app relies on has been updated by its \
             maintainers. Staying current keeps the app running smoothly.",
            "Bring your app up to date when it is convenient. This is a routine \
             improvement, nothing urgent.",
        ),
        MaintenanceFinding::new(
            "stub-sec-patch-1",
            MaintenanceKind::SecurityPatch,
            Severity::Critical,
            "A security fix is available for part of your app.",
            "A security issue was found and fixed in a component your app uses. \
             Applying the fix keeps your app and its data safe.",
            "It is a good idea to bring your app up to date. A part of it has a \
             security fix available.",
        ),
        MaintenanceFinding::new(
            "stub-key-rotation-1",
            MaintenanceKind::KeyRotation,
            Severity::Important,
            "One of your app credentials is due for its regular rotation.",
            "Rotating credentials on a regular schedule is a standard security \
             practice. A credential your app uses has passed its scheduled interval.",
            "Schedule a credential rotation soon. The app will continue working in the \
             meantime; this is a planned hygiene step.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn fixed_now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap()
    }

    #[tokio::test]
    async fn stub_scanner_returns_expected_shape() {
        let scanner = StubScanner::new();
        let scan = scanner.scan("app-abc", fixed_now()).await.unwrap();

        assert_eq!(scan.app_id, "app-abc");
        assert_eq!(scan.scanned_at, fixed_now());
        // Default stub has exactly three findings.
        assert_eq!(scan.findings.len(), 3);
        // One of each kind.
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

    #[tokio::test]
    async fn stub_scanner_respects_passed_timestamp() {
        let t1 = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        let t2 = Utc.with_ymd_and_hms(2026, 6, 14, 0, 0, 0).unwrap();
        let scanner = StubScanner::new();
        let s1 = scanner.scan("app-x", t1).await.unwrap();
        let s2 = scanner.scan("app-x", t2).await.unwrap();
        assert_eq!(s1.scanned_at, t1);
        assert_eq!(s2.scanned_at, t2);
    }

    #[tokio::test]
    async fn stub_scanner_empty_variant_has_no_findings() {
        let scanner = StubScanner::new().empty();
        let scan = scanner.scan("app-empty", fixed_now()).await.unwrap();
        assert!(scan.findings.is_empty());
    }

    #[tokio::test]
    async fn stub_scanner_custom_findings_override_defaults() {
        let custom = MaintenanceFinding::new(
            "custom-1",
            MaintenanceKind::Backup,
            Severity::Info,
            "A routine backup check passed.",
            "Your app backup looks healthy.",
            "No action needed.",
        );
        let scanner = StubScanner::new().with_findings(vec![custom.clone()]);
        let scan = scanner.scan("app-y", fixed_now()).await.unwrap();
        assert_eq!(scan.findings.len(), 1);
        assert_eq!(scan.findings[0], custom);
    }

    #[test]
    fn scan_has_security_true_when_security_finding_present() {
        let scan = MaintenanceScan::new(
            "app-1",
            fixed_now(),
            vec![MaintenanceFinding::new(
                "s1",
                MaintenanceKind::SecurityPatch,
                Severity::Critical,
                "summary",
                "detail",
                "rec",
            )],
        );
        assert!(scan.has_security());
    }

    #[test]
    fn scan_has_security_false_when_no_security_finding() {
        let scan = MaintenanceScan::new(
            "app-1",
            fixed_now(),
            vec![MaintenanceFinding::new(
                "d1",
                MaintenanceKind::DependencyUpgrade,
                Severity::Recommended,
                "summary",
                "detail",
                "rec",
            )],
        );
        assert!(!scan.has_security());
    }

    #[test]
    fn scan_highest_severity_correct() {
        let now = fixed_now();
        let empty = MaintenanceScan::new("app-1", now, vec![]);
        assert_eq!(empty.highest_severity(), None);

        let scan = MaintenanceScan::new(
            "app-2",
            now,
            vec![
                MaintenanceFinding::new(
                    "d1",
                    MaintenanceKind::DependencyUpgrade,
                    Severity::Recommended,
                    "s",
                    "d",
                    "r",
                ),
                MaintenanceFinding::new(
                    "s1",
                    MaintenanceKind::SecurityPatch,
                    Severity::Critical,
                    "s",
                    "d",
                    "r",
                ),
                MaintenanceFinding::new(
                    "k1",
                    MaintenanceKind::KeyRotation,
                    Severity::Important,
                    "s",
                    "d",
                    "r",
                ),
            ],
        );
        assert_eq!(scan.highest_severity(), Some(Severity::Critical));
    }

    #[test]
    fn scan_security_findings_returns_only_security() {
        let now = fixed_now();
        let scan = MaintenanceScan::new(
            "app-3",
            now,
            vec![
                MaintenanceFinding::new(
                    "d1",
                    MaintenanceKind::DependencyUpgrade,
                    Severity::Recommended,
                    "s",
                    "d",
                    "r",
                ),
                MaintenanceFinding::new(
                    "s1",
                    MaintenanceKind::SecurityPatch,
                    Severity::Critical,
                    "s",
                    "d",
                    "r",
                ),
            ],
        );
        let sec = scan.security_findings();
        assert_eq!(sec.len(), 1);
        assert_eq!(sec[0].id, "s1");
    }

    #[test]
    fn scan_round_trip_json() {
        let scan = MaintenanceScan::new(
            "app-rt",
            fixed_now(),
            vec![MaintenanceFinding::new(
                "f1",
                MaintenanceKind::CertRenewal,
                Severity::Important,
                "Certificate approaching expiry.",
                "Your certificate will expire within 30 days.",
                "Renew your certificate soon.",
            )],
        );
        let json = serde_json::to_string(&scan).unwrap();
        let back: MaintenanceScan = serde_json::from_str(&json).unwrap();
        assert_eq!(back, scan);
    }
}
