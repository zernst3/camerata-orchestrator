//! The one object that holds every count of the client package (single counts, work order A).
//!
//! Three units, never mixed: a row (one line the engine produced), a place (one repo, file and
//! line; "problem" in a client sentence is a place with a problem) and a fix (one cause, one
//! change). [`report_counts`] works every number out once from the final reviewed rows, after
//! every decision, merge, hold and severity bound is applied. The names are the client words of
//! `docs/glossary-operator.md`.
//!
//! The five identities hold by construction and are asserted in debug builds:
//!
//! 1. `rows.total = dependency + ruled_out + problem + convention + not_checked`
//! 2. `problems.total = needs_decision + for_action + accepted + information_only`
//! 3. `rows.problem + problems.beside_ruled_out = problems.total + rows.folded`
//! 4. `for_action = first + next + later`
//! 5. `problems.total = critical + high + medium + low`
//!
//! A place of another rule that a ruled-out row carries and that no problem row already holds
//! (`open_beside_ruled_out`) is a problem that needs a decision (fail closed): it has no row of
//! its own, so `problems.beside_ruled_out` counts it, at the severity of the ruled-out row that
//! carries it (the host's), never at the lowest.
//!
//! A place needs a decision when ANY row there is held (fail closed); "information only" is its
//! own class, apart from needs a decision. A place whose rows are accepted and information only
//! counts as information only: nobody has settled it.

use serde::{Deserialize, Serialize};

use super::ReportOptions;
use crate::review_tree::{place_key, places_of, PlaceClass};
use crate::xlsx_export::{CountKind, CountingView};

/// Every count of the package. Serialised whole as `counts` in the PDF data and `findings.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportCounts {
    pub rows: RowCounts,
    pub problems: ProblemCounts,
    pub fixes: FixCounts,
    pub other: OtherCounts,
    /// What was read in person, and the review budget.
    #[serde(default)]
    pub review: ReviewCounts,
}

/// What a person read, and the review budget that bounds it. The PDF and the workbook print these
/// words from here ([`crate::xlsx_export::vocabulary::review_line`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewCounts {
    /// Places (problems) where any row was reviewed in person: the number of "Reviewed in
    /// person" cells marked on the All problems sheet.
    pub reviewed_in_person: usize,
    /// How many places the review budget allows a person to read: from the review tree. `None`
    /// for a report with no review tree.
    pub review_budget: Option<usize>,
    /// No finding shown at critical severity and no finding the finder was unsure of lacks a
    /// reviewer's decision of its own. The release gate refuses a package where this is false, so
    /// a client package states what a person read only when it is true
    /// ([`crate::release_gate::needs_decision`]).
    pub every_critical_and_unsure_decided: bool,
}

/// Rows: for the reconciliation line and the tool files only, never a client headline.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowCounts {
    /// Every row of the reviewed report, dependency advisories included.
    pub total: usize,
    /// Rows of the dependency rule.
    pub dependency: usize,
    /// Rows a reviewer or the engine ruled out.
    pub ruled_out: usize,
    /// Defect and departure rows not ruled out.
    pub problem: usize,
    /// Problem rows that are not the head of their place.
    pub folded: usize,
    /// Convention and no convention rows not ruled out.
    pub convention: usize,
    /// Rows for a rule that could not be completed, not ruled out.
    pub not_checked: usize,
    /// Rows found by an outside tool, not reviewed by the model (not ruled out).
    pub tool_preview: usize,
}

/// Problems by the worst severity at the place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SeverityCounts {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
}

/// Places (a problem is a place with at least one problem row).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProblemCounts {
    pub total: usize,
    pub by_severity: SeverityCounts,
    /// Problems where ANY row is held for a reviewer.
    pub needs_decision: usize,
    /// Problems not needing a decision with at least one row First, Next or Later.
    pub for_action: usize,
    pub first: usize,
    pub next: usize,
    pub later: usize,
    /// Problems whose every row is accepted risk.
    pub accepted: usize,
    /// Problems with no row to act on and none held, at least one information only.
    pub information_only: usize,
    /// Problems where any row was sent for review in person.
    pub escalated: usize,
    /// Problems that need a decision, by the worst severity at the place (they add up to
    /// `needs_decision`). The grid names them when it leaves them out.
    #[serde(default)]
    pub needs_decision_by_severity: SeverityCounts,
    /// Problems that are information only, by the worst severity at the place.
    #[serde(default)]
    pub information_only_by_severity: SeverityCounts,
    /// Problems with no row of their own: a place of another rule that a ruled-out row carries
    /// and that stays open. Each is also counted in `needs_decision`.
    #[serde(default)]
    pub beside_ruled_out: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FixCounts {
    /// Every fix.
    pub total: usize,
    /// Fixes with at least one place or advisory still to change: the Fix plan rows.
    pub to_do: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OtherCounts {
    pub files_scanned: usize,
    pub repositories: usize,
    /// Rules the inspection declared it checked.
    pub rules_checked: usize,
    /// Distinct rules with at least one problem row.
    pub rules_with_problems: usize,
    /// Waiver comments and baseline entries found in the client's repository and not honoured
    /// by a first inspection. The problems they cover are open.
    #[serde(default)]
    pub client_suppressions_found: usize,
}

impl SeverityCounts {
    /// One more problem of `severity` (anything but critical, high and medium is low).
    pub fn add(&mut self, severity: &str) {
        match severity {
            "critical" => self.critical += 1,
            "high" => self.high += 1,
            "medium" => self.medium += 1,
            _ => self.low += 1,
        }
    }

    /// The sum of the four.
    pub fn sum(&self) -> usize {
        self.critical + self.high + self.medium + self.low
    }
}

impl ReportCounts {
    /// The four identities, as the list of those that do not hold (empty when all hold).
    pub fn identity_failures(&self) -> Vec<&'static str> {
        let (r, p) = (&self.rows, &self.problems);
        let mut out = Vec::new();
        if r.total != r.dependency + r.ruled_out + r.problem + r.convention + r.not_checked {
            out.push("source rows are not dependency + ruled out + problem + convention + not checked rows");
        }
        if p.total != p.needs_decision + p.for_action + p.accepted + p.information_only {
            out.push(
                "problems are not needs a decision + for action + accepted + information only",
            );
        }
        if r.problem + p.beside_ruled_out != p.total + r.folded {
            out.push(
                "problem rows and places beside a ruled out row are not problems + rows at a shared place",
            );
        }
        if p.for_action != p.first + p.next + p.later {
            out.push("for action is not first + next + later");
        }
        if p.total
            != p.by_severity.critical
                + p.by_severity.high
                + p.by_severity.medium
                + p.by_severity.low
        {
            out.push("problems are not critical + high + medium + low");
        }
        out
    }
}

/// The counts of `reviewed`, worked out once (see the module doc).
pub fn report_counts(
    reviewed: &crate::report_export::ReviewedReport,
    corpus: Option<&camerata_rules::RuleSet>,
    opts: &ReportOptions,
) -> ReportCounts {
    let view =
        crate::xlsx_export::counting_view(reviewed, corpus, &opts.chosen_options, opts.internal);
    let report = reviewed.report();
    let fixes = crate::review_tree::FixIndex::new(report.tree.as_ref(), &report.findings);
    let fixes_total = fixes.fixes().len();
    let fixes_to_do = fixes
        .fixes()
        .iter()
        .filter(|f| view.fix_ids_to_do.contains(&f.id))
        .count();
    let mut counts = counts_of(
        &view,
        FixCounts {
            total: fixes_total,
            to_do: fixes_to_do,
        },
        OtherCounts {
            files_scanned: report.files_scanned,
            repositories: report.repos.len(),
            rules_checked: report.provenance.audited_rule_ids.len(),
            rules_with_problems: 0,
            client_suppressions_found: report.ledger.client_suppressions_found(),
        },
    );
    counts.review.review_budget = report.tree.as_ref().map(|t| t.budget.budget);
    counts.review.every_critical_and_unsure_decided =
        crate::release_gate::needs_decision::undecided(&report.findings, reviewed)
            .next()
            .is_none();
    counts
}

/// The counts of a counting view; split from [`report_counts`] so the arithmetic is testable on
/// plain rows.
pub(crate) fn counts_of(
    view: &CountingView,
    fixes: FixCounts,
    mut other: OtherCounts,
) -> ReportCounts {
    let live = || view.rows.iter().filter(|r| !r.ruled_out);
    let problem_rows: Vec<_> = live().filter(|r| r.kind == CountKind::Problem).collect();
    let places = places_of(problem_rows.iter().copied());

    let mut rows = RowCounts {
        dependency: view.dependency_rows,
        ruled_out: view.rows.iter().filter(|r| r.ruled_out).count(),
        problem: problem_rows.len(),
        convention: live().filter(|r| r.kind == CountKind::Convention).count(),
        not_checked: live().filter(|r| r.kind == CountKind::NotChecked).count(),
        tool_preview: live().filter(|r| r.tool_preview).count(),
        ..RowCounts::default()
    };
    rows.total = view.rows.len() + view.dependency_rows;
    rows.folded = places.iter().map(|p| p.members.len() - 1).sum();

    let mut p = ProblemCounts {
        total: places.len(),
        ..ProblemCounts::default()
    };
    let reviewed_in_person = places
        .iter()
        .filter(|place| place.members.iter().any(|m| m.reviewed_in_person))
        .count();
    for place in &places {
        p.by_severity.add(&place.head.severity);
        match place.class {
            PlaceClass::NeedsDecision => {
                p.needs_decision += 1;
                p.needs_decision_by_severity.add(&place.head.severity);
            }
            PlaceClass::ForAction(order) => {
                p.for_action += 1;
                match order {
                    crate::review_tree::FixOrder::First => p.first += 1,
                    crate::review_tree::FixOrder::Next => p.next += 1,
                    crate::review_tree::FixOrder::Later => p.later += 1,
                }
            }
            PlaceClass::InformationOnly => {
                p.information_only += 1;
                p.information_only_by_severity.add(&place.head.severity);
            }
            PlaceClass::AcceptedRisk => p.accepted += 1,
        }
        if place.members.iter().any(|m| m.escalated) {
            p.escalated += 1;
        }
    }
    // Fail closed: an open place a ruled-out row carries, that no problem row already holds, is
    // a problem that needs a decision.
    let held_places: std::collections::HashSet<_> = places
        .iter()
        .map(|pl| place_key(&pl.head.repo, &pl.head.path, pl.head.line, &pl.head.rule_id))
        .collect();
    let mut beside: std::collections::HashSet<(&str, &str, usize, &str)> =
        std::collections::HashSet::new();
    for site in &view.open_beside_ruled_out {
        let key = place_key(&site.repo, &site.path, site.line, &site.rule_id);
        if !held_places.contains(&key) && beside.insert(key) {
            p.total += 1;
            p.needs_decision += 1;
            p.by_severity.add(&site.severity);
            p.needs_decision_by_severity.add(&site.severity);
            p.beside_ruled_out += 1;
        }
    }
    let distinct_rules: std::collections::BTreeSet<&str> =
        problem_rows.iter().map(|r| r.rule_id.as_str()).collect();
    other.rules_with_problems = distinct_rules.len();

    let counts = ReportCounts {
        rows,
        problems: p,
        fixes,
        other,
        review: ReviewCounts {
            reviewed_in_person,
            ..ReviewCounts::default()
        },
    };
    debug_assert!(
        counts.identity_failures().is_empty(),
        "report counts break an identity: {:?}",
        counts.identity_failures()
    );
    counts
}
