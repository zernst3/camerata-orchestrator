//! The release gate holds a client package while a rule is split between options with no
//! convention and no ruling (Zach's ruling on amendment A3, 2026-11-20). The reviewer decides it
//! either way: accept an option (the convention is then set by the reviewer) or choose "Leave to
//! the client" (the report lists the options as an open question). The set of rules comes from
//! [`crate::conventions::split_rules_without_choice`], the one function the export dialog's count
//! reads too.

use super::Problem;
use crate::report_export::ReviewedReport;

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
                "findings.json",
                format!("{repo} {rule}"),
                format!(
                    "the rule {rule} is split between options and has no decision. Recommend an \
                     option for it, or leave it to the client, then export again"
                ),
                Some(rule),
            )
        })
        .collect()
}
