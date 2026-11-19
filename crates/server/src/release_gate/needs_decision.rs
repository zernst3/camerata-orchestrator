//! Outside review item 2: a critical finding and an unsure finding do not ship without a
//! reviewer's decision.
//!
//! The site says every critical finding and every call the engine is not sure of is reviewed in
//! person. The review budget alone does not make that true: a critical whose reason is not one
//! of the severe ones can sit below the line with "Reviewed in person" blank. This check closes
//! it, in the same shape as [`super::evidence`]: it asks one question of a
//! [`ReviewerDecisions`], so moving to the decision store changes nothing here.
//!
//! It does not force every high: the site does not claim it.
//!
//! # What "critical" and "unsure" mean here
//!
//! - Critical: the row's severity as the exports show it. The scan applied every rule's severity
//!   bound and the overlay applied the reviewer's severity edits, so the row's own severity is
//!   what the PDF and the workbook print (the exporters re-apply the same bounds, which change
//!   nothing a second time). The bounds that need no rule (the deviation ceiling and the
//!   unlocated downgrade) are applied here too, as the exporters apply them.
//! - Unsure: the finder or the group review marked the row unsure (`needs_review`, or a
//!   needs-review confidence). A row the workbook also prints as Low only because it sits in the
//!   held or information-only bucket (no published standard backs it, or it reads as a convention)
//!   was not marked unsure by a review and is not forced: the wording says "marked unsure".
//!
//! A decision made only as part of a group does not count for a critical or an unsure row: the
//! effective record must be the row's own (`inherited_from` empty), and every such row is kept out
//! of group decisions ([`super::holds`]), decided or not, so a later group decision cannot
//! overwrite a decision made in person.

use super::evidence::ReviewerDecisions;
use super::Problem;
use crate::onboard::Finding;
use crate::report_export::ReviewedReport;

/// Why a row needs a decision of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Why {
    /// The row's severity is critical.
    Critical,
    /// The finder was unsure the row is a real problem.
    Unsure,
}

/// The reason a row needs a decision, when it does. A critical that is also unsure is reported as
/// critical, the stronger of the two.
pub fn why(f: &Finding) -> Option<Why> {
    if crate::severity_bounds::bounded_severity(f, None) == "critical" {
        Some(Why::Critical)
    } else if f.needs_review || f.confidence.as_deref() == Some("needs-review") {
        Some(Why::Unsure)
    } else {
        None
    }
}

/// Whether the reviewer decided `f` in person: the effective record for the row (the latest
/// live one) is its own, not one inherited from a group decision. A group record that came after
/// the reviewer's own overwrites it, and then the row is undecided again. With no record at all
/// (an older client's request dispositions) the row's own disposition counts.
fn decided_in_person(f: &Finding, reviewed: &ReviewedReport) -> bool {
    let effective = camerata_api_types::decisions::effective_by_key(reviewed.records());
    match effective.get(f.site_key.as_str()) {
        Some(record) => record.inherited_from.is_none(),
        None => reviewed.has_decision(f),
    }
}

/// The rows shown at critical severity or marked unsure that `reviewed` has no decision of their
/// own for, with why each needs one: what [`critical_or_unsure_without_decision`] refuses, as
/// rows. The export panel lists these same rows ([`super::holds`]).
pub fn undecided<'a>(
    findings: &'a [Finding],
    reviewed: &'a ReviewedReport,
) -> impl Iterator<Item = (&'a Finding, Why)> {
    findings
        .iter()
        .filter_map(|f| why(f).map(|w| (f, w)))
        .filter(|(f, _)| !decided_in_person(f, reviewed))
}

/// [`undecided`], and also every row the exports SHOW at critical (`shown_critical`, the Place ids
/// from [`crate::xlsx_export::FindingsExport::shown_critical_site_ids`]) whatever its stored
/// severity says: a bound applied only at export time cannot hide a critical from the gate.
fn undecided_as_shown<'a>(
    findings: &'a [Finding],
    reviewed: &'a ReviewedReport,
    shown_critical: &'a std::collections::HashSet<&'a str>,
) -> impl Iterator<Item = (&'a Finding, Why)> {
    findings
        .iter()
        .filter_map(move |f| {
            let id = crate::report_export::site_id(&f.site_key);
            let critical = shown_critical.contains(id.as_str());
            why(f)
                .or_else(|| critical.then_some(Why::Critical))
                .map(|w| (f, w))
        })
        .filter(|(f, _)| !decided_in_person(f, reviewed))
}

/// One problem per row shown at critical severity or marked unsure that has no decision. The
/// release gate's own list: it reads the severity the package shows
/// (`shown_critical`, from the findings export) as well as the stored one.
pub fn critical_or_unsure_without_decision(
    findings: &[Finding],
    reviewed: &ReviewedReport,
    shown_critical: &std::collections::HashSet<&str>,
) -> Vec<Problem> {
    undecided_as_shown(findings, reviewed, shown_critical)
        .map(|(f, why)| {
            let what = match why {
                Why::Critical => "this finding is shown at critical severity",
                Why::Unsure => "the finder was unsure this finding is a real problem",
            };
            Problem::new(
                "findings.json",
                format!("{} line {}", f.path, f.line),
                format!(
                    "{what}, and no reviewer decision is recorded for it. Open it, then confirm \
                     it, correct it or mark it a false positive"
                ),
                Some(f.rule_id.clone()),
            )
        })
        .collect()
}
