//! Onboarding-triage state machine, extracted from the Dioxus scan adapter (RUST-HEADLESS-CORE-1).
//!
//! This module owns the PURE triage model: the data shapes (`FindingView`, `Disposition`), the
//! stable-identity + state-derivation helpers (`finding_key`, `finding_state`), and the
//! framework-agnostic state machine (`TriageModel`) that the Dioxus layer drives. The adapter
//! keeps the reactive signals, the chorale table effects, the toasts, and the rsx; every pure
//! transition + derivation lives here and is unit-tested with no VirtualDom.
//!
//! The architect moves each finding between four tables (Unresolved / Ignored / Tech debt /
//! False positive) until nothing is Unresolved, then buckets the tech-debt findings (resolve
//! later / now). This is LOCAL triage state — the backend commit (baseline waiver / ticket /
//! dev-engine import) happens at Process, not on each move. False positive is the odd one out:
//! it is never committed anywhere — see `TriageState::FalsePositive`'s doc comment.

use std::collections::HashMap;

/// Where a finding sits in onboarding triage. The architect moves each finding between these
/// FOUR tables (a single-select switches the view) until nothing is Unresolved; then the
/// ignored and tech-debt buckets are processed. This is LOCAL triage state — the backend
/// commit (baseline waiver / ticket / dev-engine import) happens at Process, not on each move.
///
/// `FalsePositive` is NOT "accepted risk" like `Ignored` — it means the tool was WRONG. It
/// never reaches the baseline and never files a ticket; at Process it is a pure client-local
/// no-op (see the Process handler in `cockpit/scan.rs`), counted once in the report's
/// Methodology section and otherwise excluded entirely. Durably suppressing a false positive
/// via the baseline would be exactly the invisible, reasonless-waiver hole the require-reason
/// baseline invariant prevents — if the same code trips again next scan, that is correct: the
/// reviewer re-judges it fresh.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug, serde::Serialize, serde::Deserialize)]
pub enum TriageState {
    #[default]
    Unresolved,
    Ignored,
    TechDebt,
    FalsePositive,
}

impl TriageState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Unresolved => "Unresolved",
            Self::Ignored => "Ignored",
            Self::TechDebt => "Tech debt",
            Self::FalsePositive => "False positive",
        }
    }
}

/// Which tech-debt bucket a finding is in: resolve LATER (file a tracked ticket) or NOW (pull
/// into the dev engine as the first story). Only meaningful when state == TechDebt.
#[derive(Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TechDebtBucket {
    Later,
    Now,
}

/// One finding's triage disposition: its table, the (required) ignore reason, and its
/// tech-debt bucket. Absence from the dispositions map == Unresolved with defaults.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Disposition {
    pub state: TriageState,
    pub reason: String,
    pub bucket: TechDebtBucket,
}

impl Default for Disposition {
    fn default() -> Self {
        Self {
            state: TriageState::Unresolved,
            reason: String::new(),
            bucket: TechDebtBucket::Later,
        }
    }
}

/// One audit finding, as it deserializes off the BFF scan/audit response. Wide, plain serde
/// DTO — no transport concern, no rendering concern. Shared by the CSV export, the findings
/// table, and the triage model.
#[derive(Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct FindingView {
    #[serde(default)]
    pub repo: String,
    pub path: String,
    pub line: usize,
    pub rule_id: String,
    pub severity: String,
    pub snippet: String,
    pub detail: String,
    /// `active` (enforced), `suppressed-inline`, or `suppressed-baseline`.
    #[serde(default = "default_finding_status")]
    pub status: String,
    /// Other rule ids this same location also violates (the server merged them into this
    /// row). Empty for an un-merged finding. Surfaced as a "+N" on the rule and listed in
    /// the detail modal.
    #[serde(default)]
    pub also_matches: Vec<String>,
    /// PREVIEW (CI-security Part B): the server's scan-time deterministic preview pass ran
    /// the rule's underlying tool ITSELF and produced this finding, even though the rule is
    /// NOT yet wired into the repo's gate. Deterministic (stable tool rule-id) but ADVISORY:
    /// "preview — not enforced until wired". Defaults to `false` (back-compatible).
    #[serde(default)]
    pub preview: bool,
    /// For a preview finding, the tool that produced it (`clippy` | `ruff` | `eslint` |
    /// `semgrep`). `None` for non-preview findings. Shown in the Authority badge label.
    #[serde(default)]
    pub preview_tool: Option<String>,
    /// True when this finding is in test/fixture scope.
    #[serde(default)]
    pub in_test: bool,
    /// True when this finding needs manual verification.
    #[serde(default)]
    pub needs_review: bool,
    /// Structured calibration confidence: `"high"` | `"needs-review"`. `None` for findings
    /// calibration never saw (the deterministic floor, preview findings). Mirrors
    /// `camerata_server::onboard::Finding::confidence` on the wire — kept in sync so a field
    /// added server-side is never silently dropped by serde here (see the round-trip
    /// contract tests at the bottom of this module).
    #[serde(default)]
    pub confidence: Option<String>,
    /// Structured remediation-effort estimate: `"low"` | `"medium"` | `"high"`. `None` for
    /// findings calibration never saw. Mirrors `camerata_server::onboard::Finding::effort`.
    #[serde(default)]
    pub effort: Option<String>,
}

/// The default finding status (`"active"`). FindingView's `#[serde(default)]` provider; lives in
/// the scan surface (re-exported here so `FindingView`'s serde attribute resolves within core).
pub use crate::scan::default_finding_status;

/// Stable identity for a finding across the triage tables (repo + rule + location + snippet),
/// so its disposition survives table switches and re-sorts.
pub fn finding_key(f: &FindingView) -> String {
    format!(
        "{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
        f.repo, f.rule_id, f.path, f.line, f.snippet
    )
}

/// The disposition state for a finding (Unresolved when absent from the map).
pub fn finding_state(dispositions: &HashMap<String, Disposition>, f: &FindingView) -> TriageState {
    dispositions
        .get(&finding_key(f))
        .map(|d| d.state)
        .unwrap_or(TriageState::Unresolved)
}

/// The framework-agnostic onboarding-triage state machine. Owns the finding -> disposition map
/// plus the currently-viewed triage table, and exposes the pure transitions + derivations the
/// Dioxus layer drives (hooks/table/toast stay OUTSIDE; the mutation happens INSIDE). Extracted
/// verbatim from the scan adapter's onclick handlers + inline filter/sort.
pub struct TriageModel {
    pub dispositions: HashMap<String, Disposition>,
    pub triage_view: TriageState,
}

impl TriageModel {
    pub fn new() -> Self {
        Self {
            dispositions: HashMap::new(),
            triage_view: TriageState::Unresolved,
        }
    }

    /// The disposition state for a finding under this model's map (Unresolved when absent).
    pub fn state_of(&self, f: &FindingView) -> TriageState {
        finding_state(&self.dispositions, f)
    }

    /// Move each finding to `state` (its bucket/reason are left as-is / defaulted on insert).
    pub fn move_to(&mut self, findings: &[FindingView], state: TriageState) {
        for f in findings {
            self.dispositions.entry(finding_key(f)).or_default().state = state;
        }
    }

    /// Ignore each finding: set state = Ignored AND record the (required) reason together, so a
    /// durable ignore never lands without its baseline-recorded reason.
    pub fn ignore(&mut self, findings: &[FindingView], reason: &str) {
        for f in findings {
            let e = self.dispositions.entry(finding_key(f)).or_default();
            e.state = TriageState::Ignored;
            e.reason = reason.to_string();
        }
    }

    /// Mark each finding as a FALSE POSITIVE: set state = FalsePositive AND record the
    /// (required) reason together — mirrors `ignore`'s require-reason invariant. The reason
    /// is the audit trail (why the tool was wrong) and feeds the report's methodology count
    /// ("N dispositioned as false positives by the auditor and excluded"). Unlike `ignore`,
    /// this disposition is a Process-time NO-OP: it never reaches the baseline and never
    /// files a ticket (see the Process handler in `cockpit/scan.rs`).
    pub fn mark_false_positive(&mut self, findings: &[FindingView], reason: &str) {
        for f in findings {
            let e = self.dispositions.entry(finding_key(f)).or_default();
            e.state = TriageState::FalsePositive;
            e.reason = reason.to_string();
        }
    }

    /// Bucket each tech-debt finding (resolve later / now). Leaves the state unchanged; only the
    /// Bucket flag moves.
    pub fn set_bucket(&mut self, findings: &[FindingView], bucket: TechDebtBucket) {
        for f in findings {
            self.dispositions.entry(finding_key(f)).or_default().bucket = bucket;
        }
    }

    /// The findings visible in the current `triage_view`: filtered to that table's state, then
    /// ordered active-before-suppressed, then by severity (critical > high > medium > other).
    pub fn visible(&self, findings: &[FindingView]) -> Vec<FindingView> {
        let mut out: Vec<FindingView> = findings
            .iter()
            .filter(|f| self.state_of(f) == self.triage_view)
            .cloned()
            .collect();
        out.sort_by_key(|f| {
            let enforced = if f.status == "active" { 0 } else { 1 };
            let sev = match f.severity.as_str() {
                "critical" => 0,
                "high" => 1,
                "medium" => 2,
                _ => 3,
            };
            (enforced, sev)
        });
        out
    }

    /// Tally the four buckets over `findings`: `(Unresolved, Ignored, TechDebt, FalsePositive)`.
    pub fn counts(&self, findings: &[FindingView]) -> (usize, usize, usize, usize) {
        let mut unresolved = 0;
        let mut ignored = 0;
        let mut tech_debt = 0;
        let mut false_positive = 0;
        for f in findings {
            match self.state_of(f) {
                TriageState::Unresolved => unresolved += 1,
                TriageState::Ignored => ignored += 1,
                TriageState::TechDebt => tech_debt += 1,
                TriageState::FalsePositive => false_positive += 1,
            }
        }
        (unresolved, ignored, tech_debt, false_positive)
    }
}

impl Default for TriageModel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(json: serde_json::Value) -> FindingView {
        serde_json::from_value(json).expect("valid FindingView fixture")
    }

    // ── moved from the scan adapter (path prefix changed only) ────────────────

    #[test]
    fn finding_key_combines_identity_fields() {
        let f = finding(serde_json::json!({
            "repo": "owner/repo", "path": "src/a.rs", "line": 7,
            "rule_id": "RULE-X", "severity": "high", "snippet": "snip", "detail": ""
        }));
        let key = finding_key(&f);
        assert!(key.contains("owner/repo"));
        assert!(key.contains("RULE-X"));
        assert!(key.contains("src/a.rs"));
        assert!(key.contains('7'));
        assert!(key.contains("snip"));
    }

    #[test]
    fn finding_state_defaults_to_unresolved_when_absent() {
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1,
            "rule_id": "R", "severity": "low", "snippet": "s", "detail": ""
        }));
        let map = HashMap::new();
        assert_eq!(finding_state(&map, &f), TriageState::Unresolved);
    }

    #[test]
    fn finding_state_reads_disposition_when_present() {
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1,
            "rule_id": "R", "severity": "low", "snippet": "s", "detail": ""
        }));
        let mut map = HashMap::new();
        map.insert(
            finding_key(&f),
            Disposition {
                state: TriageState::Ignored,
                reason: "noise".to_string(),
                bucket: TechDebtBucket::Later,
            },
        );
        assert_eq!(finding_state(&map, &f), TriageState::Ignored);
    }

    #[test]
    fn default_finding_status_is_active() {
        assert_eq!(default_finding_status(), "active");
    }

    #[test]
    fn triage_state_defaults_to_unresolved() {
        assert_eq!(TriageState::default(), TriageState::Unresolved);
    }

    // ── new: the TriageModel state machine ────────────────────────────────────

    fn f(repo: &str, line: usize, severity: &str, status: &str) -> FindingView {
        finding(serde_json::json!({
            "repo": repo, "path": "p", "line": line,
            "rule_id": "R", "severity": severity, "snippet": "s", "detail": "", "status": status
        }))
    }

    #[test]
    fn disposition_default_is_unresolved_empty_later() {
        let d = Disposition::default();
        assert_eq!(d.state, TriageState::Unresolved);
        assert_eq!(d.reason, "");
        // TechDebtBucket derives no Debug (kept identical to the pre-extraction type), so compare
        // via PartialEq rather than assert_eq!'s Debug formatting.
        assert!(d.bucket == TechDebtBucket::Later);
    }

    #[test]
    fn move_to_round_trips_state() {
        let mut m = TriageModel::new();
        let item = f("r", 1, "low", "active");
        assert_eq!(m.state_of(&item), TriageState::Unresolved);
        m.move_to(std::slice::from_ref(&item), TriageState::TechDebt);
        assert_eq!(m.state_of(&item), TriageState::TechDebt);
        m.move_to(std::slice::from_ref(&item), TriageState::Unresolved);
        assert_eq!(m.state_of(&item), TriageState::Unresolved);
    }

    #[test]
    fn ignore_sets_state_and_reason_together() {
        let mut m = TriageModel::new();
        let item = f("r", 1, "low", "active");
        m.ignore(std::slice::from_ref(&item), "noise");
        let d = m.dispositions.get(&finding_key(&item)).expect("recorded");
        assert_eq!(d.state, TriageState::Ignored);
        assert_eq!(d.reason, "noise");
    }

    #[test]
    fn set_bucket_flips_later_and_now_via_or_default() {
        let mut m = TriageModel::new();
        let item = f("r", 1, "low", "active");
        // First set on an absent key goes through or_default (starts Later), then flips to Now.
        m.set_bucket(std::slice::from_ref(&item), TechDebtBucket::Now);
        assert!(m.dispositions.get(&finding_key(&item)).unwrap().bucket == TechDebtBucket::Now);
        m.set_bucket(std::slice::from_ref(&item), TechDebtBucket::Later);
        assert!(m.dispositions.get(&finding_key(&item)).unwrap().bucket == TechDebtBucket::Later);
    }

    #[test]
    fn visible_filters_to_view_and_sorts() {
        let mut m = TriageModel::new();
        let unresolved_crit = f("r", 1, "critical", "active");
        let unresolved_low = f("r", 2, "low", "active");
        let unresolved_high_suppressed = f("r", 3, "high", "suppressed-baseline");
        let ignored = f("r", 4, "critical", "active");
        m.ignore(std::slice::from_ref(&ignored), "noise");

        let all = vec![
            unresolved_low.clone(),
            unresolved_high_suppressed.clone(),
            unresolved_crit.clone(),
            ignored.clone(),
        ];

        // Unresolved view: the ignored finding is filtered out.
        let vis = m.visible(&all);
        assert_eq!(vis.len(), 3);
        // active-before-suppressed, then critical > high > medium > low:
        // active/critical (line 1), active/low (line 2), suppressed/high (line 3).
        assert_eq!(vis[0].line, 1);
        assert_eq!(vis[1].line, 2);
        assert_eq!(vis[2].line, 3);

        // Ignored view shows only the ignored finding.
        m.triage_view = TriageState::Ignored;
        let vis = m.visible(&all);
        assert_eq!(vis.len(), 1);
        assert_eq!(vis[0].line, 4);
    }

    #[test]
    fn counts_tallies_the_four_buckets() {
        let mut m = TriageModel::new();
        let a = f("r", 1, "low", "active");
        let b = f("r", 2, "low", "active");
        let c = f("r", 3, "low", "active");
        let d = f("r", 4, "low", "active");
        m.ignore(std::slice::from_ref(&b), "noise");
        m.move_to(std::slice::from_ref(&c), TriageState::TechDebt);
        m.mark_false_positive(std::slice::from_ref(&d), "not exploitable — mocked in test scope");
        let all = vec![a, b, c, d];
        assert_eq!(m.counts(&all), (1, 1, 1, 1));
    }

    // ── FalsePositive disposition (the explicit ask) ──────────────────────────

    #[test]
    fn false_positive_label_reads_false_positive() {
        assert_eq!(TriageState::FalsePositive.label(), "False positive");
    }

    #[test]
    fn mark_false_positive_sets_state_and_reason_together() {
        let mut m = TriageModel::new();
        let item = f("r", 1, "low", "active");
        m.mark_false_positive(std::slice::from_ref(&item), "tool misfired on generated code");
        let d = m.dispositions.get(&finding_key(&item)).expect("recorded");
        assert_eq!(d.state, TriageState::FalsePositive);
        assert_eq!(d.reason, "tool misfired on generated code");
    }

    #[test]
    fn mark_false_positive_is_distinct_from_ignore() {
        // FalsePositive and Ignored must never collapse into the same state — a false
        // positive is "the tool was wrong" (excluded, no baseline entry), Ignored is
        // "real but accepted risk" (committed to the baseline). Mixing them up would be
        // dishonest in both directions.
        let mut m = TriageModel::new();
        let fp = f("r", 1, "low", "active");
        let ignored = f("r", 2, "low", "active");
        m.mark_false_positive(std::slice::from_ref(&fp), "false alarm");
        m.ignore(std::slice::from_ref(&ignored), "accepted for now");
        assert_eq!(m.state_of(&fp), TriageState::FalsePositive);
        assert_eq!(m.state_of(&ignored), TriageState::Ignored);
        assert_ne!(TriageState::FalsePositive, TriageState::Ignored);
    }

    #[test]
    fn visible_filters_false_positive_into_its_own_view() {
        let mut m = TriageModel::new();
        let unresolved = f("r", 1, "high", "active");
        let fp = f("r", 2, "critical", "active");
        m.mark_false_positive(std::slice::from_ref(&fp), "generated fixture, not real code");
        let all = vec![unresolved.clone(), fp.clone()];

        // Unresolved view excludes the false positive.
        let vis = m.visible(&all);
        assert_eq!(vis.len(), 1);
        assert_eq!(vis[0].line, 1);

        // FalsePositive view shows only the false positive.
        m.triage_view = TriageState::FalsePositive;
        let vis = m.visible(&all);
        assert_eq!(vis.len(), 1);
        assert_eq!(vis[0].line, 2);
    }

    // ── FindingView confidence/effort mirror (server `Finding` sync) ──────────

    #[test]
    fn finding_view_confidence_and_effort_default_to_none() {
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1,
            "rule_id": "R", "severity": "low", "snippet": "s", "detail": ""
        }));
        assert_eq!(f.confidence, None);
        assert_eq!(f.effort, None);
    }

    #[test]
    fn finding_view_deserializes_confidence_and_effort_when_present() {
        let f = finding(serde_json::json!({
            "repo": "r", "path": "p", "line": 1,
            "rule_id": "R", "severity": "high", "snippet": "s", "detail": "",
            "confidence": "needs-review", "effort": "high"
        }));
        assert_eq!(f.confidence.as_deref(), Some("needs-review"));
        assert_eq!(f.effort.as_deref(), Some("high"));
    }
}
