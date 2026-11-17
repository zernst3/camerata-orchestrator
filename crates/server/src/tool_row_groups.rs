//! External tool rows share one cause per tool rule (calibration round 2, change A).
//!
//! A tool such as eslint reports one rule at many places. Design amendment A11 keeps one row per
//! site, so every place stays its own row (this module never writes `also_locations`), and the
//! client counts CAUSES (distinct Fix ids). A Fix is a set of sub-groups, and a row's sub-group is
//! the third segment of its `group_path` (`review_tree::build`), so a cause is shared by giving
//! rows the same `group_path`:
//!
//! * every row of one (repo, tool, tool rule), the camerata-arch rows included, takes one
//!   `group_path`, so they are one sub-group and one Fix;
//! * when the rule family (`review_tree::family_of`) matches an agent row in the same repository,
//!   the tool rows take that agent row's `group_path` instead and join its cause. The agent row's
//!   rating is untouched, and the tool is named in `also_seen_by` on each tool row.
//!
//! Style-lint rows are rated low ([`rate_style_lint_rows`]); a lint's security checks keep the
//! tool's own severity.

use crate::onboard::Finding;
use std::collections::{BTreeMap, HashMap};

/// Rate a style linter's row low: its severity is a strictness preference, not a judgement about
/// harm. Its security checks (an `eslint-plugin-security` id, a Ruff `S` rule) keep the tool's.
fn rate_style_lint_low(f: &mut Finding) {
    if !crate::is_style_linter_row(f)
        || crate::lint_id_is_security(f)
        || f.severity.eq_ignore_ascii_case("low")
        || f.severity.eq_ignore_ascii_case("info")
    {
        return;
    }
    let from = f.severity.clone();
    f.severity = "low".to_string();
    crate::severity_bounds::record_adjustment(
        f,
        format!(
            "severity lowered from {from} to low: a style linter's label is a strictness \
             preference, not a judgement about harm"
        ),
    );
}

/// [`rate_style_lint_low`] over a batch of rows.
pub(crate) fn rate_style_lint_rows(rows: &mut [Finding]) {
    rows.iter_mut().for_each(rate_style_lint_low);
}

/// A complete, well-formed place: a category node, a rule node and a sub-group node.
fn is_whole_path(path: &[String]) -> bool {
    matches!(path, [c, r, s] if c.starts_with("cat:") && r.starts_with("rule:") && s.starts_with("sub:"))
}

/// The place an agent row has, or will get when the tree is built (the residual sub-group of its
/// rule).
fn cause_path_of(agent: &Finding, tools: &crate::review_tree::ToolMap) -> Vec<String> {
    if is_whole_path(&agent.group_path) {
        return agent.group_path.clone();
    }
    let placed = crate::review_tree::place_row(agent, tools);
    let sub = crate::review_tree::residual_sub_id(&placed.rule);
    vec![placed.category, placed.rule, sub]
}

/// Give every row of one (repo, tool, tool rule) one shared place in the tree (one cause), or the
/// place of the agent row whose rule family it matches.
pub(crate) fn share_tool_causes(
    findings: &mut [Finding],
    selectors: Option<&HashMap<String, Vec<String>>>,
) {
    let tools = crate::review_tree::ToolMap::from_selectors(selectors);
    let mut groups: BTreeMap<(String, String, String), Vec<usize>> = BTreeMap::new();
    for (i, f) in findings.iter().enumerate() {
        if let Some(tool) = &f.preview_tool {
            groups
                .entry((f.repo.clone(), tool.clone(), f.rule_id.clone()))
                .or_default()
                .push(i);
        }
    }
    for ((repo, tool, _rule), mut idxs) in groups {
        idxs.sort_by(|&a, &b| {
            (&findings[a].path, findings[a].line).cmp(&(&findings[b].path, findings[b].line))
        });
        let head = &findings[idxs[0]];
        let family = crate::review_tree::family_of(head, &tools);
        let agent = findings.iter().find(|a| {
            a.captures
                .contains_key(crate::ai_audit::agent_finder::reply::FINDER_PROBLEM_CAPTURE)
                && a.repo == repo
                && (crate::review_tree::family_of(a, &tools) == family
                    || crate::preview_is_native_rule_tool(&head.rule_id, &a.rule_id, selectors))
        });
        let (path, folded) = match agent {
            Some(a) => (Some(cause_path_of(a, &tools)), true),
            None => (
                idxs.iter()
                    .map(|&i| &findings[i].group_path)
                    .find(|p| is_whole_path(p))
                    .cloned(),
                false,
            ),
        };
        let Some(path) = path else { continue };
        for &i in &idxs {
            findings[i].group_path = path.clone();
            if folded {
                crate::note_also_seen_by(&mut findings[i], &tool);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::review_tree::FixIndex;

    fn tool_row(tool: &str, rule: &str, path: &str, line: usize, sev: &str) -> Finding {
        Finding {
            repo: "o/r".to_string(),
            path: path.to_string(),
            line,
            rule_id: rule.to_string(),
            severity: sev.to_string(),
            snippet: format!("code at {path}:{line}"),
            preview: true,
            preview_tool: Some(tool.to_string()),
            kind: Some(camerata_api_types::audit::FindingKind::Defect),
            ..Finding::default()
        }
    }

    fn agent_row(rule: &str, path: &str, line: usize, sev: &str) -> Finding {
        let mut f = Finding {
            repo: "o/r".to_string(),
            path: path.to_string(),
            line,
            rule_id: rule.to_string(),
            severity: sev.to_string(),
            snippet: format!("agent code at {path}:{line}"),
            kind: Some(camerata_api_types::audit::FindingKind::Defect),
            ..Finding::default()
        };
        f.captures.insert(
            crate::ai_audit::agent_finder::reply::FINDER_PROBLEM_CAPTURE.to_string(),
            "1".to_string(),
        );
        f
    }

    /// The report as the scan has it: the audit's rows, the tool rows joined, the tree built.
    fn joined(existing: Vec<Finding>, previews: Vec<Finding>) -> crate::onboard::ScanReport {
        let mut report = crate::release_gate::test_scan_report();
        report.findings = existing;
        crate::join_repo_previews(&mut report, previews, None, None, &[], None);
        report.assign_site_keys();
        report
    }

    fn fix_ids(report: &crate::onboard::ScanReport, rule: &str) -> Vec<String> {
        let index = FixIndex::new(report.tree.as_ref(), &report.findings);
        report
            .findings
            .iter()
            .filter(|f| f.rule_id == rule)
            .map(|f| index.fix_id_of(f))
            .collect()
    }

    #[test]
    fn twenty_five_eslint_rows_of_one_rule_stay_rows_low_and_share_one_fix() {
        let rows: Vec<Finding> = (0..25)
            .map(|i| {
                tool_row(
                    "eslint",
                    "no-unused-vars",
                    &format!("src/f{i:02}.js"),
                    3,
                    "medium",
                )
            })
            .collect();
        let report = joined(Vec::new(), rows);
        assert_eq!(report.findings.len(), 25, "every place stays its own row");
        assert!(report.findings.iter().all(|f| f.severity == "low"));
        assert!(report.findings.iter().all(|f| f.also_locations.is_empty()));
        let ids = fix_ids(&report, "no-unused-vars");
        assert!(!ids[0].is_empty());
        assert!(ids.iter().all(|i| *i == ids[0]), "one cause: {ids:?}");
    }

    #[test]
    fn an_eslint_security_rule_keeps_its_own_severity() {
        let rows = vec![tool_row(
            "eslint",
            "security/detect-eval-with-expression",
            "a.js",
            1,
            "high",
        )];
        let report = joined(Vec::new(), rows);
        assert_eq!(report.findings[0].severity, "high");
    }

    #[test]
    fn two_tool_rules_are_two_fixes() {
        let rows = vec![
            tool_row("eslint", "no-unused-vars", "a.js", 1, "low"),
            tool_row("eslint", "no-undef", "b.js", 2, "low"),
        ];
        let report = joined(Vec::new(), rows);
        let a = fix_ids(&report, "no-unused-vars");
        let b = fix_ids(&report, "no-undef");
        assert_ne!(a[0], b[0]);
    }

    #[test]
    fn a_semgrep_row_in_the_family_of_an_agent_row_shares_the_agent_rows_fix() {
        let agent = agent_row("SEC-NO-UNSAFE-DESERIALIZATION-1", "src/a.rs", 9, "high");
        let previews = vec![
            tool_row(
                "semgrep",
                "camerata.security.yaml-unsafe-load",
                "src/x.rs",
                4,
                "high",
            ),
            tool_row(
                "semgrep",
                "camerata.security.yaml-unsafe-load",
                "src/y.rs",
                8,
                "high",
            ),
        ];
        let report = joined(vec![agent], previews);
        assert_eq!(report.findings.len(), 3, "every place stays its own row");
        let agent_fix = fix_ids(&report, "SEC-NO-UNSAFE-DESERIALIZATION-1");
        let tool_fix = fix_ids(&report, "camerata.security.yaml-unsafe-load");
        assert!(!agent_fix[0].is_empty());
        assert_eq!(tool_fix, vec![agent_fix[0].clone(), agent_fix[0].clone()]);
        let agent_row = report
            .findings
            .iter()
            .find(|f| f.rule_id == "SEC-NO-UNSAFE-DESERIALIZATION-1")
            .unwrap();
        assert_eq!(
            agent_row.severity, "high",
            "the agent's rating is untouched"
        );
        assert!(report
            .findings
            .iter()
            .filter(|f| f.preview_tool.is_some())
            .all(|f| f
                .captures
                .get(crate::ALSO_SEEN_BY_CAPTURE)
                .map(String::as_str)
                == Some("semgrep")));
    }

    #[test]
    fn a_camerata_arch_rule_with_no_agent_row_is_its_own_single_fix() {
        let agent = agent_row("SEC-NO-UNSAFE-DESERIALIZATION-1", "src/a.rs", 9, "high");
        let arch: Vec<Finding> = (0..3)
            .map(|i| {
                tool_row(
                    crate::onboard::ARCH_PREVIEW_TOOL,
                    "ARCH-LAYERING-1",
                    &format!("src/l{i}.rs"),
                    2,
                    "high",
                )
            })
            .collect();
        let report = joined(vec![agent], arch);
        let arch_fix = fix_ids(&report, "ARCH-LAYERING-1");
        let agent_fix = fix_ids(&report, "SEC-NO-UNSAFE-DESERIALIZATION-1");
        assert!(arch_fix.iter().all(|i| *i == arch_fix[0] && !i.is_empty()));
        assert_ne!(arch_fix[0], agent_fix[0]);
        assert!(report
            .findings
            .iter()
            .filter(|f| f.rule_id == "ARCH-LAYERING-1")
            .all(|f| f.severity == "high"));
    }

    #[test]
    fn a_stored_report_reloads_with_the_same_rows() {
        let rows: Vec<Finding> = (0..5)
            .map(|i| {
                tool_row(
                    "eslint",
                    "no-unused-vars",
                    &format!("src/f{i}.js"),
                    3,
                    "medium",
                )
            })
            .collect();
        let mut report = joined(Vec::new(), rows);
        let before: Vec<(String, usize, Vec<String>)> = report
            .findings
            .iter()
            .map(|f| (f.path.clone(), f.line, f.group_path.clone()))
            .collect();
        report.normalize_loaded();
        let after: Vec<(String, usize, Vec<String>)> = report
            .findings
            .iter()
            .map(|f| (f.path.clone(), f.line, f.group_path.clone()))
            .collect();
        assert_eq!(before, after);
    }
}
