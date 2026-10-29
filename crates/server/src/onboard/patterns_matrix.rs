//! Patterns across services: where the services of one project differ on a rule.
//!
//! A project of several repos is inspected one repo at a time, so every finding is a statement
//! about one service. This module reads the scan's per-(rule, repo) facts side by side and notes
//! the rules that are violated in at least one service and clean in at least one other. It is an
//! OBSERVATION, never a finding:
//!
//! * no severity, nothing in any risk total or finding count, never "consolidate" advice and
//!   never a judgement about whether the difference is intended;
//! * a row appears only when the rule is [`CellState::Violated`] in at least one repo and
//!   [`CellState::Clean`] in at least one other;
//! * the other two states, [`CellState::NotApplicable`] and [`CellState::NotReviewed`], are kept
//!   in the grid as what they are and never folded into clean. A rule that is simply missing a
//!   finding for a repo is not clean: the cell comes from [`ScanLedger::cell_state`], which
//!   reads "nothing was recorded" as not reviewed.
//!
//! The cell states are the ledger's own ([`CellState`] is defined once, in `scan_ledger`); this
//! module only refines a cell with the report's findings, which can only move a cell toward
//! violated (a finding is a finding) or away from clean (a check that could not be completed is
//! not a clean check).
//!
//! An optional model pass can group the rows into themes ([`Theme`]). It sees only the rows and
//! one headline per violated cell, never code, and [`keep_valid_themes`] drops any theme that
//! cites a rule or service the matrix does not hold.

use std::collections::BTreeSet;

use camerata_api_types::audit::FindingKind;
use serde::{Deserialize, Serialize};

use super::ScanReport;
use crate::scan_ledger::CellState;

/// The sentence every pattern row is introduced with. Fixed wording: it states a difference and
/// names the two ways forward, and says nothing about which side is right.
pub fn pattern_sentence(title: &str) -> String {
    format!(
        "These services handle {title} differently. If that is intended, record it as a \
         convention; if not, a shared library or a written standard would close the gap."
    )
}

/// One service's cell in a pattern row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoRuleCell {
    pub repo: String,
    /// The ledger's cell state, flattened into the cell (`state`, plus `sites` or `reason`).
    #[serde(flatten)]
    pub state: CellState,
}

/// One rule that differs between services: a cell per service of the project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternRow {
    pub rule_id: String,
    pub cells: Vec<RepoRuleCell>,
}

impl PatternRow {
    /// The services where the rule is violated, with the site count.
    pub fn violated(&self) -> Vec<(&str, usize)> {
        self.cells
            .iter()
            .filter_map(|c| match &c.state {
                CellState::Violated { sites } => Some((c.repo.as_str(), *sites)),
                _ => None,
            })
            .collect()
    }

    /// The services where the rule was checked and is clean.
    pub fn clean(&self) -> Vec<&str> {
        self.cells
            .iter()
            .filter(|c| c.state == CellState::Clean)
            .map(|c| c.repo.as_str())
            .collect()
    }

    /// The cell for one service.
    pub fn cell(&self, repo: &str) -> Option<&RepoRuleCell> {
        self.cells.iter().find(|c| c.repo == repo)
    }
}

/// A group of rows a model pass found a common subject in. Every rule and service it names
/// exists in the matrix ([`keep_valid_themes`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub rule_ids: Vec<String>,
    pub repos: Vec<String>,
    pub note: String,
}

/// The matrix: the services compared, the rows where they differ and the optional themes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatternsAcrossServices {
    /// The services compared, in the order the scan listed them. Fewer than two means there was
    /// nothing to compare and the section does not exist.
    #[serde(default)]
    pub repos: Vec<String>,
    #[serde(default)]
    pub rows: Vec<PatternRow>,
    #[serde(default)]
    pub themes: Vec<Theme>,
}

impl PatternsAcrossServices {
    /// Whether the project has more than one service, so the section (and its sheet and tab)
    /// exists. A one-repo project has none.
    pub fn is_present(&self) -> bool {
        self.repos.len() > 1
    }
}

/// Whether a finding states a violation. A convention the service follows and a check that could
/// not be completed are not violations; a missing convention is a gap, so it is.
fn is_violation(kind: Option<&FindingKind>) -> bool {
    !matches!(
        kind,
        Some(FindingKind::ConventionObserved(_) | FindingKind::NotAssessed(_))
    )
}

/// The ledger's cell for (rule, repo), refined with the report's findings for it.
pub(crate) fn cell_for(report: &ScanReport, rule_id: &str, repo: &str) -> CellState {
    let applies = report.ledger.rule_applies_to_repo(rule_id, repo);
    let base = report.ledger.cell_state(rule_id, repo, applies);
    let mut violations = 0usize;
    let mut convention_seen = false;
    let mut not_assessed = false;
    for f in report.findings.iter().filter(|f| f.repo == repo) {
        if f.rule_id.eq_ignore_ascii_case(rule_id) {
            match f.kind.as_ref() {
                Some(FindingKind::ConventionObserved(_)) => convention_seen = true,
                Some(FindingKind::NotAssessed(_)) => not_assessed = true,
                other => violations += usize::from(is_violation(other)),
            }
        } else if f
            .also_matches
            .iter()
            .any(|m| m.eq_ignore_ascii_case(rule_id))
        {
            violations += 1;
        }
    }
    // A site a merge absorbed into another rule's row still fired, and may live in another repo
    // than the row that absorbed it.
    for f in &report.findings {
        violations += f
            .also_locations
            .iter()
            .filter(|l| l.repo == repo && l.rule_id.eq_ignore_ascii_case(rule_id))
            .count();
    }
    if violations > 0 {
        return CellState::Violated { sites: violations };
    }
    let not_completed = || CellState::NotReviewed {
        reason: "the check could not be completed on this service".to_string(),
    };
    match base {
        CellState::Violated { sites } => {
            if not_assessed {
                not_completed()
            } else if convention_seen {
                CellState::Clean
            } else {
                CellState::Violated { sites }
            }
        }
        CellState::Clean if not_assessed => not_completed(),
        other => other,
    }
}

/// Build the matrix for a report. Empty (not present) for a project of fewer than two repos.
/// The dependency audit is left out: an advisory against a package is not a way of doing
/// something that one service could adopt from another.
pub fn build(report: &ScanReport) -> PatternsAcrossServices {
    let mut repos: Vec<String> = Vec::new();
    for repo in report
        .repos
        .iter()
        .chain(report.repos_not_inspected.iter().map(|(repo, _)| repo))
    {
        if !repo.is_empty() && !repos.contains(repo) {
            repos.push(repo.clone());
        }
    }
    if repos.len() < 2 {
        return PatternsAcrossServices::default();
    }
    let rule_ids: BTreeSet<String> = report
        .ledger
        .rules()
        .map(|e| e.rule_id.to_ascii_uppercase())
        .filter(|id| id != crate::dep_audit::DEP_AUDIT_RULE_ID)
        .collect();
    let mut rows = Vec::new();
    for rule_id in rule_ids {
        let cells: Vec<RepoRuleCell> = repos
            .iter()
            .map(|repo| RepoRuleCell {
                repo: repo.clone(),
                state: cell_for(report, &rule_id, repo),
            })
            .collect();
        let violated = cells
            .iter()
            .any(|c| matches!(c.state, CellState::Violated { .. }));
        let clean = cells.iter().any(|c| c.state == CellState::Clean);
        if violated && clean {
            rows.push(PatternRow { rule_id, cells });
        }
    }
    PatternsAcrossServices {
        repos,
        rows,
        themes: Vec::new(),
    }
}

/// The themes that cite only what the matrix holds. A theme is kept when it has a name, cites at
/// least one rule and one service, every rule it cites is a row, every service it cites is a
/// service of the matrix, and the rules and services it cites are tied together by cells that
/// are violated or clean: each cited rule has such a cell for a cited service and each cited
/// service has such a cell for a cited rule. Anything else is a model's invention and is dropped.
pub fn keep_valid_themes(themes: Vec<Theme>, matrix: &PatternsAcrossServices) -> Vec<Theme> {
    themes
        .into_iter()
        .filter(|theme| theme_is_valid(theme, matrix))
        .collect()
}

fn theme_is_valid(theme: &Theme, matrix: &PatternsAcrossServices) -> bool {
    if theme.name.trim().is_empty() || theme.rule_ids.is_empty() || theme.repos.is_empty() {
        return false;
    }
    if !theme.repos.iter().all(|r| matrix.repos.contains(r)) {
        return false;
    }
    let mut rows: Vec<&PatternRow> = Vec::new();
    for id in &theme.rule_ids {
        match matrix
            .rows
            .iter()
            .find(|row| row.rule_id.eq_ignore_ascii_case(id))
        {
            Some(row) => rows.push(row),
            None => return false,
        }
    }
    let informative = |row: &PatternRow, repo: &str| {
        row.cell(repo)
            .is_some_and(|c| matches!(c.state, CellState::Violated { .. } | CellState::Clean))
    };
    rows.iter()
        .all(|row| theme.repos.iter().any(|r| informative(row, r)))
        && theme
            .repos
            .iter()
            .all(|repo| rows.iter().any(|row| informative(row, repo)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::{build_report, Finding};
    use crate::scan_ledger::RuleTier;

    const RULE: &str = "ARCH-PATTERN-TEST-1";

    fn report(repos: &[&str]) -> ScanReport {
        build_report(
            repos.iter().map(ToString::to_string).collect(),
            Vec::new(),
            0,
            Vec::new(),
        )
    }

    fn ran(r: &mut ScanReport, repo: &str, rule: &str, emitted: usize) {
        r.ledger
            .record_rule(repo, rule, RuleTier::Semantic, true, None, 5, emitted);
    }

    fn finding(repo: &str, rule: &str, kind: Option<FindingKind>) -> Finding {
        Finding {
            repo: repo.to_string(),
            path: "src/index.ts".to_string(),
            line: 3,
            rule_id: rule.to_string(),
            severity: "medium".to_string(),
            kind,
            ..Finding::default()
        }
    }

    fn state(m: &PatternsAcrossServices, rule: &str, repo: &str) -> CellState {
        m.rows
            .iter()
            .find(|r| r.rule_id == rule)
            .and_then(|r| r.cell(repo))
            .map(|c| c.state.clone())
            .unwrap_or_else(|| panic!("no cell for {rule} in {repo}"))
    }

    #[test]
    fn a_rule_violated_in_one_service_and_clean_in_another_is_one_row() {
        let mut r = report(&["acme/a", "acme/b"]);
        ran(&mut r, "acme/a", RULE, 2);
        ran(&mut r, "acme/b", RULE, 0);
        r.findings.push(finding("acme/a", RULE, None));
        r.findings.push(finding("acme/a", RULE, None));
        let m = build(&r);
        assert_eq!(m.rows.len(), 1);
        assert_eq!(state(&m, RULE, "acme/a"), CellState::Violated { sites: 2 });
        assert_eq!(state(&m, RULE, "acme/b"), CellState::Clean);
        assert_eq!(m.rows[0].violated(), vec![("acme/a", 2)]);
        assert_eq!(m.rows[0].clean(), vec!["acme/b"]);
    }

    #[test]
    fn a_rule_violated_in_one_service_and_not_applicable_in_another_is_no_row() {
        let mut r = report(&["acme/a", "acme/b"]);
        ran(&mut r, "acme/a", RULE, 1);
        r.findings.push(finding("acme/a", RULE, None));
        r.ledger.record_rule(
            "acme/b",
            RULE,
            RuleTier::Semantic,
            false,
            Some("no rust files matched this scan's file set".to_string()),
            0,
            0,
        );
        assert!(build(&r).rows.is_empty());
    }

    #[test]
    fn a_service_the_rule_is_not_bound_to_is_not_applicable_and_never_clean() {
        let mut r = report(&["acme/a", "acme/b", "acme/c"]);
        r.ledger
            .note_rule_scope(RULE, &["acme/a".to_string(), "acme/b".to_string()]);
        ran(&mut r, "acme/a", RULE, 1);
        ran(&mut r, "acme/b", RULE, 0);
        r.findings.push(finding("acme/a", RULE, None));
        let m = build(&r);
        assert!(matches!(
            state(&m, RULE, "acme/c"),
            CellState::NotApplicable { .. }
        ));
    }

    #[test]
    fn a_service_with_nothing_recorded_is_not_reviewed_never_clean() {
        let mut r = report(&["acme/a", "acme/b", "acme/c"]);
        ran(&mut r, "acme/a", RULE, 1);
        ran(&mut r, "acme/b", RULE, 0);
        r.findings.push(finding("acme/a", RULE, None));
        let m = build(&r);
        assert!(matches!(
            state(&m, RULE, "acme/c"),
            CellState::NotReviewed { .. }
        ));
    }

    #[test]
    fn a_missing_finding_alone_never_makes_a_clean_cell() {
        // Violated in a, and b has no ledger entry at all: no clean cell, so no row.
        let mut r = report(&["acme/a", "acme/b"]);
        ran(&mut r, "acme/a", RULE, 1);
        r.findings.push(finding("acme/a", RULE, None));
        assert!(build(&r).rows.is_empty());
        // A repo whose review failed is not reviewed, so it is no clean cell either.
        r.ledger.record_rule(
            "acme/b",
            RULE,
            RuleTier::Semantic,
            false,
            Some(format!(
                "{}: boom",
                crate::scan_ledger::AI_AUDIT_FAILED_MARKER
            )),
            0,
            0,
        );
        assert!(build(&r).rows.is_empty());
    }

    #[test]
    fn a_check_that_ran_on_part_of_the_code_is_not_reviewed() {
        let mut r = report(&["acme/a", "acme/b", "acme/c"]);
        ran(&mut r, "acme/a", RULE, 1);
        r.findings.push(finding("acme/a", RULE, None));
        ran(&mut r, "acme/b", RULE, 0);
        ran(&mut r, "acme/c", RULE, 0);
        r.ledger
            .note_partial_rule(RULE, "acme/c", "spend limit reached");
        let m = build(&r);
        assert_eq!(state(&m, RULE, "acme/b"), CellState::Clean);
        assert!(matches!(
            state(&m, RULE, "acme/c"),
            CellState::NotReviewed { .. }
        ));
    }

    #[test]
    fn a_followed_convention_is_clean_and_a_check_that_could_not_complete_is_not() {
        use camerata_api_types::audit::{
            ConventionObservedKind, NotAssessedKind, NotAssessedReason,
        };
        let mut r = report(&["acme/a", "acme/b", "acme/c"]);
        ran(&mut r, "acme/a", RULE, 1);
        r.findings.push(finding("acme/a", RULE, None));
        ran(&mut r, "acme/b", RULE, 1);
        r.findings.push(finding(
            "acme/b",
            RULE,
            Some(FindingKind::ConventionObserved(ConventionObservedKind {
                convention_option_id: "cursor".to_string(),
                convention_label: "cursor pagination".to_string(),
            })),
        ));
        ran(&mut r, "acme/c", RULE, 1);
        r.findings.push(finding(
            "acme/c",
            RULE,
            Some(FindingKind::NotAssessed(NotAssessedKind {
                reason: NotAssessedReason::PassFailed,
                reason_detail: String::new(),
            })),
        ));
        let m = build(&r);
        assert_eq!(state(&m, RULE, "acme/b"), CellState::Clean);
        assert!(matches!(
            state(&m, RULE, "acme/c"),
            CellState::NotReviewed { .. }
        ));
    }

    #[test]
    fn a_finding_makes_a_cell_violated_even_when_the_ledger_says_clean() {
        let mut r = report(&["acme/a", "acme/b"]);
        ran(&mut r, "acme/a", RULE, 0);
        ran(&mut r, "acme/b", RULE, 0);
        r.findings.push(finding("acme/a", RULE, None));
        let m = build(&r);
        assert_eq!(state(&m, RULE, "acme/a"), CellState::Violated { sites: 1 });
    }

    #[test]
    fn a_site_absorbed_into_another_rules_row_still_counts_for_its_own_repo() {
        let mut r = report(&["acme/a", "acme/b"]);
        ran(&mut r, "acme/a", RULE, 1);
        ran(&mut r, "acme/a", "OTHER-RULE-1", 1);
        ran(&mut r, "acme/b", RULE, 0);
        let mut host = finding("acme/a", "OTHER-RULE-1", None);
        host.also_locations = vec![crate::onboard::MergedLocation::site(
            "acme/a", "src/x.ts", 9, RULE, "x", false,
        )];
        r.findings.push(host);
        let m = build(&r);
        assert_eq!(state(&m, RULE, "acme/a"), CellState::Violated { sites: 1 });
    }

    #[test]
    fn the_dependency_audit_is_not_a_pattern() {
        let mut r = report(&["acme/a", "acme/b"]);
        ran(&mut r, "acme/a", crate::dep_audit::DEP_AUDIT_RULE_ID, 1);
        ran(&mut r, "acme/b", crate::dep_audit::DEP_AUDIT_RULE_ID, 0);
        r.findings
            .push(finding("acme/a", crate::dep_audit::DEP_AUDIT_RULE_ID, None));
        assert!(build(&r).rows.is_empty());
    }

    #[test]
    fn one_repo_has_no_matrix() {
        let mut r = report(&["acme/a"]);
        ran(&mut r, "acme/a", RULE, 1);
        r.findings.push(finding("acme/a", RULE, None));
        let m = build(&r);
        assert!(!m.is_present());
        assert!(m.rows.is_empty());
    }

    fn matrix_with_one_row() -> PatternsAcrossServices {
        let mut r = report(&["acme/a", "acme/b", "acme/c"]);
        ran(&mut r, "acme/a", RULE, 1);
        r.findings.push(finding("acme/a", RULE, None));
        ran(&mut r, "acme/b", RULE, 0);
        r.ledger.record_rule(
            "acme/c",
            RULE,
            RuleTier::Semantic,
            false,
            Some("no rust files matched this scan's file set".to_string()),
            0,
            0,
        );
        build(&r)
    }

    fn theme(rules: &[&str], repos: &[&str]) -> Theme {
        Theme {
            name: "Pagination".to_string(),
            rule_ids: rules.iter().map(ToString::to_string).collect(),
            repos: repos.iter().map(ToString::to_string).collect(),
            note: "n".to_string(),
        }
    }

    #[test]
    fn a_theme_citing_only_what_the_matrix_holds_is_kept() {
        let m = matrix_with_one_row();
        let kept = keep_valid_themes(vec![theme(&[RULE], &["acme/a", "acme/b"])], &m);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn a_theme_citing_a_rule_a_service_or_a_cell_the_matrix_lacks_is_dropped() {
        let m = matrix_with_one_row();
        // A rule that is not a row.
        assert!(keep_valid_themes(vec![theme(&["MADE-UP-1"], &["acme/a"])], &m).is_empty());
        // A service that is not in the matrix.
        assert!(keep_valid_themes(vec![theme(&[RULE], &["acme/ghost"])], &m).is_empty());
        // A service whose only cell for the rule is not applicable.
        assert!(keep_valid_themes(vec![theme(&[RULE], &["acme/c"])], &m).is_empty());
        // Nothing cited.
        assert!(keep_valid_themes(vec![theme(&[], &["acme/a"])], &m).is_empty());
        assert!(keep_valid_themes(vec![theme(&[RULE], &[])], &m).is_empty());
        // One good theme among bad ones survives alone.
        let kept = keep_valid_themes(
            vec![
                theme(&["MADE-UP-1"], &["acme/a"]),
                theme(&[RULE], &["acme/a"]),
            ],
            &m,
        );
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].rule_ids, vec![RULE.to_string()]);
    }

    #[test]
    fn the_sentence_is_the_fixed_wording_with_no_dashes_and_no_serial_comma() {
        let s = pattern_sentence("pagination");
        assert_eq!(
            s,
            "These services handle pagination differently. If that is intended, record it as a \
             convention; if not, a shared library or a written standard would close the gap."
        );
        assert!(!s.contains('\u{2014}') && !s.contains('\u{2013}'));
    }

    #[test]
    fn a_cell_serializes_with_its_state_flattened() {
        let cell = RepoRuleCell {
            repo: "acme/a".to_string(),
            state: CellState::Violated { sites: 3 },
        };
        let v = serde_json::to_value(&cell).unwrap();
        assert_eq!(v["repo"], "acme/a");
        assert_eq!(v["state"], "violated");
        assert_eq!(v["sites"], 3);
        let back: RepoRuleCell = serde_json::from_value(v).unwrap();
        assert_eq!(back, cell);
        let na = serde_json::to_value(RepoRuleCell {
            repo: "r".to_string(),
            state: CellState::NotApplicable {
                reason: "x".to_string(),
            },
        })
        .unwrap();
        assert_eq!(na["state"], "not_applicable");
        assert_eq!(na["reason"], "x");
    }
}
