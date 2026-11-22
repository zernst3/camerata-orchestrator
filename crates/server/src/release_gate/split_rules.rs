//! The release gate holds a client package while a rule is split between options with no
//! convention and no ruling (Zach's ruling on amendment A3, 2026-11-20). The reviewer decides it
//! either way: accept an option (the convention is then set by the reviewer) or choose "Leave to
//! the client" (the report lists the options as an open question). The set of rules comes from
//! [`crate::conventions::split_rules_without_choice`], the one function the export dialog's count
//! reads too.

use super::Problem;
use crate::report_export::ReviewedReport;
use camerata_api_types::package_paths::ALL_FINDINGS_JSON;

/// What every split-rule problem's message contains; a caller that must tell these problems from
/// the gate's others (the CLI's hint) looks for it.
pub const SPLIT_PROBLEM_MARK: &str = "is split between options and has no decision";

/// The rules the gate holds for `reviewed`, as (repo, rule id).
pub fn split_rules(reviewed: &ReviewedReport) -> Vec<(String, String)> {
    crate::conventions::split_rules_without_choice(&reviewed.report().conventions)
        .into_iter()
        .map(|c| (c.repo.clone(), c.rule_id.clone()))
        .collect()
}

/// One problem for each rule split with no decision, naming the rule.
pub fn problems(reviewed: &ReviewedReport) -> Vec<Problem> {
    split_rules(reviewed)
        .into_iter()
        .map(|(repo, rule)| {
            Problem::new(
                ALL_FINDINGS_JSON,
                format!("{repo} {rule}"),
                format!(
                    "the rule {rule} {SPLIT_PROBLEM_MARK}. Accept an \
                     option for it, or leave it to the client, then export again"
                ),
                Some(rule),
            )
        })
        .collect()
}
