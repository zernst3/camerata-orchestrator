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
//! its own, so `problems.beside_ruled_out` counts it, and it counts at the lowest severity, the
//! rank an unknown severity has everywhere.
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
    counts_of(
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
        },
    )
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
    for place in &places {
        match place.head.severity.as_str() {
            "critical" => p.by_severity.critical += 1,
            "high" => p.by_severity.high += 1,
            "medium" => p.by_severity.medium += 1,
            _ => p.by_severity.low += 1,
        }
        match place.class {
            PlaceClass::NeedsDecision => p.needs_decision += 1,
            PlaceClass::ForAction(order) => {
                p.for_action += 1;
                match order {
                    crate::review_tree::FixOrder::First => p.first += 1,
                    crate::review_tree::FixOrder::Next => p.next += 1,
                    crate::review_tree::FixOrder::Later => p.later += 1,
                }
            }
            PlaceClass::InformationOnly => p.information_only += 1,
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
            p.by_severity.low += 1;
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
    };
    debug_assert!(
        counts.identity_failures().is_empty(),
        "report counts break an identity: {:?}",
        counts.identity_failures()
    );
    counts
}
