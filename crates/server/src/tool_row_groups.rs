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

/// The tools whose rows share one cause per tool rule: the style and security linters. The
/// camerata-arch rows are per-site detectors; they stay as the audit and the group reviewer
/// placed them.
const CAUSE_TOOLS: &[&str] = &["eslint", "ruff", "clippy", "semgrep"];

fn is_cause_tool(tool: &str) -> bool {
    CAUSE_TOOLS.iter().any(|t| t.eq_ignore_ascii_case(tool))
}

/// The step that decides the tool causes. It runs AFTER the group reviewer, which re-sorts the
/// rows of a rule into its own sub-groups, so it is the one place that decides them. Rebuilds
/// the review tree when it moved any row, so the tree and the Fixes agree with the rows.
pub fn settle_tool_causes(
    report: &mut crate::onboard::ScanReport,
    corpus: Option<&camerata_rules::RuleSet>,
) {
    let selectors = corpus.map(crate::native_tool_selectors);
    if share_tool_causes(&mut report.findings, selectors.as_ref()) {
        report.attach_tree();
    }
}

/// The next unused sub-group id under `rule_node` (`rule:X` gives `sub:X/N`).
fn fresh_sub(findings: &[Finding], rule_node: &str) -> String {
    let bare = rule_node.strip_prefix("rule:").unwrap_or(rule_node);
    let prefix = format!("sub:{bare}/");
    let top = findings
        .iter()
        .filter_map(|f| f.group_path.get(2))
        .filter_map(|s| s.strip_prefix(&prefix))
        .filter_map(|n| n.parse::<u32>().ok())
        .max()
        .unwrap_or(0);
    format!("{prefix}{}", top + 1)
}

/// Give every row of one (repo, tool, tool rule) of a style or security linter one shared place
/// in the tree (one cause). Returns whether any row moved.
///
/// * The family has exactly one agent cause in the repository: the rows take that cause.
/// * The family has several agent causes: the rows keep one cause of their own, in a sub-group
///   no agent row is in.
/// * No agent row of the family: the rows share the first place any of them already has, else
///   the residual sub-group of their rule.
pub(crate) fn share_tool_causes(
    findings: &mut [Finding],
    selectors: Option<&HashMap<String, Vec<String>>>,
) -> bool {
    let tools = crate::review_tree::ToolMap::from_selectors(selectors);
    let mut groups: BTreeMap<(String, String, String), Vec<usize>> = BTreeMap::new();
    for (i, f) in findings.iter().enumerate() {
        if let Some(tool) = f.preview_tool.as_deref().filter(|t| is_cause_tool(t)) {
            groups
                .entry((f.repo.clone(), tool.to_string(), f.rule_id.clone()))
                .or_default()
                .push(i);
        }
    }
    let mut moved = false;
    for ((repo, tool, _rule), mut idxs) in groups {
        idxs.sort_by(|&a, &b| {
            (&findings[a].path, findings[a].line).cmp(&(&findings[b].path, findings[b].line))
        });
        let head = &findings[idxs[0]];
        let family = crate::review_tree::family_of(head, &tools);
        let agent_paths: Vec<Vec<String>> = findings
            .iter()
            .filter(|a| {
                a.captures
                    .contains_key(crate::ai_audit::agent_finder::reply::FINDER_PROBLEM_CAPTURE)
                    && a.repo == repo
                    && (crate::review_tree::family_of(a, &tools) == family
                        || crate::preview_is_native_rule_tool(&head.rule_id, &a.rule_id, selectors))
            })
            .map(|a| cause_path_of(a, &tools))
            .collect();
        let causes: std::collections::BTreeSet<&String> =
            agent_paths.iter().map(|p| &p[2]).collect();
        let own = idxs
            .iter()
            .map(|&i| &findings[i].group_path)
            .find(|p| is_whole_path(p))
            .cloned()
            .unwrap_or_else(|| cause_path_of(head, &tools));
        let (path, folded) = match causes.len() {
            0 => (own, false),
            1 => (agent_paths[0].clone(), true),
            // Several agent causes: keep the rows' current sub-group when no agent row is in it
            // (so a second run changes nothing), else a fresh one.
            _ if !causes.contains(&own[2]) => (own, false),
            _ => {
                let sub = fresh_sub(findings, &own[1]);
                (vec![own[0].clone(), own[1].clone(), sub], false)
            }
        };
        for &i in &idxs {
            if findings[i].group_path != path {
                findings[i].group_path = path.clone();
                moved = true;
            }
            if folded {
                crate::note_also_seen_by(&mut findings[i], &tool);
            }
        }
    }
    moved
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
        settle_tool_causes(&mut report, None);
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

    fn placed(mut f: Finding, sub: u32) -> Finding {
        let rule = format!("rule:{}", f.rule_id);
        f.group_path = vec![
            "cat:security".to_string(),
            rule,
            format!("sub:{}/{sub}", f.rule_id),
        ];
        f
    }

    #[test]
    fn with_two_agent_causes_in_the_family_the_tool_rows_keep_their_own_single_cause() {
        let a = placed(
            agent_row("SEC-NO-UNSAFE-DESERIALIZATION-1", "src/a.rs", 9, "high"),
            1,
        );
        let b = placed(
            agent_row("SEC-NO-UNSAFE-DESERIALIZATION-1", "src/b.rs", 4, "high"),
            2,
        );
        let previews: Vec<Finding> = (0..3)
            .map(|i| {
                tool_row(
                    "semgrep",
                    "camerata.security.yaml-unsafe-load",
                    &format!("src/t{i}.rs"),
                    2,
                    "high",
                )
            })
            .collect();
        let report = joined(vec![a, b], previews);
        let agents = fix_ids(&report, "SEC-NO-UNSAFE-DESERIALIZATION-1");
        let tools = fix_ids(&report, "camerata.security.yaml-unsafe-load");
        assert_ne!(agents[0], agents[1], "two agent causes");
        assert!(
            tools.iter().all(|t| *t == tools[0] && !t.is_empty()),
            "{tools:?}"
        );
        assert!(!agents.contains(&tools[0]), "neither agent row's cause");
    }

    #[test]
    fn two_camerata_arch_rows_of_one_rule_stay_separate_causes() {
        let a = placed(
            tool_row(
                crate::onboard::ARCH_PREVIEW_TOOL,
                "ARCH-LAYERING-1",
                "src/a.rs",
                2,
                "high",
            ),
            1,
        );
        let b = placed(
            tool_row(
                crate::onboard::ARCH_PREVIEW_TOOL,
                "ARCH-LAYERING-1",
                "src/b.rs",
                3,
                "high",
            ),
            2,
        );
        let report = joined(vec![a, b], Vec::new());
        let ids = fix_ids(&report, "ARCH-LAYERING-1");
        assert_ne!(ids[0], ids[1], "the group reviewer's causes are left alone");
    }

    #[test]
    fn a_critical_row_on_a_line_the_lint_group_also_flags_keeps_its_own_fix() {
        let lint: Vec<Finding> = ["a.js", "b.js", "c.js"]
            .iter()
            .map(|p| tool_row("eslint", "no-unused-vars", p, 1, "low"))
            .collect();
        let mut secret = tool_row(
            "eslint",
            "SEC-NO-HARDCODED-SECRETS-1",
            "b.js",
            1,
            "critical",
        );
        secret.preview = false;
        secret.preview_tool = None;
        let report = joined(vec![secret], lint);
        let lint_fix = fix_ids(&report, "no-unused-vars");
        let secret_fix = fix_ids(&report, "SEC-NO-HARDCODED-SECRETS-1");
        assert_ne!(lint_fix[0], secret_fix[0]);
    }

    #[test]
    fn settling_twice_leaves_the_same_fix_ids() {
        let a = placed(
            agent_row("SEC-NO-UNSAFE-DESERIALIZATION-1", "src/a.rs", 9, "high"),
            1,
        );
        let b = placed(
            agent_row("SEC-NO-UNSAFE-DESERIALIZATION-1", "src/b.rs", 4, "high"),
            2,
        );
        let previews: Vec<Finding> = (0..3)
            .map(|i| {
                tool_row(
                    "semgrep",
                    "camerata.security.yaml-unsafe-load",
                    &format!("src/t{i}.rs"),
                    2,
                    "high",
                )
            })
            .collect();
        let mut report = joined(vec![a, b], previews);
        let ids = |r: &crate::onboard::ScanReport| -> Vec<String> {
            let index = FixIndex::new(r.tree.as_ref(), &r.findings);
            r.findings.iter().map(|f| index.fix_id_of(f)).collect()
        };
        let paths = |r: &crate::onboard::ScanReport| -> Vec<Vec<String>> {
            r.findings.iter().map(|f| f.group_path.clone()).collect()
        };
        let (before, before_paths) = (ids(&report), paths(&report));
        settle_tool_causes(&mut report, None);
        settle_tool_causes(&mut report, None);
        assert_eq!(ids(&report), before);
        assert_eq!(paths(&report), before_paths);
    }
}
