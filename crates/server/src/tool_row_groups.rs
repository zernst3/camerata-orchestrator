//! External tool rows, one row per tool rule (calibration round 2, change A).
//!
//! A tool such as eslint reports one rule at every place it fires, which used to give one report
//! row per place (25 rows of one rule). Owner ruling (2026-11-17): ONE row per tool rule with all
//! its places, folded into the agent's row when the agent already reported the same thing,
//! style-lint rules rated low, every place still counted (no open violations).
//!
//! * [`group_tool_rows`]: rows of the same (repo, tool, tool rule) become one row; the head is
//!   the first place by path and line and every other place sits in `also_locations`.
//! * [`fold_into_agent_rows`]: a grouped row whose rule is in the same family as an agent row's
//!   rule (`review_tree::family_of`) in the same repository becomes extra places on that agent
//!   row, anywhere in the repository, with the tool named in `also_seen_by`.
//!
//! A row that holds grouped places carries the `also_seen_by` mark, which the loader's unfold
//! ([`crate::review_tree::unfold_also_locations`]) reads as "these places are meant to stay".

use crate::onboard::{Finding, MergedLocation};
use std::collections::HashMap;

/// Whether `f` holds places that were grouped on purpose (see the module docs).
pub(crate) fn holds_grouped_places(f: &Finding) -> bool {
    f.captures
        .get(crate::ALSO_SEEN_BY_CAPTURE)
        .is_some_and(|v| !v.is_empty())
}

fn place_of(f: &Finding) -> MergedLocation {
    MergedLocation::site(
        f.repo.clone(),
        f.path.clone(),
        f.line,
        f.rule_id.clone(),
        f.snippet.clone(),
        false,
    )
    .with_rating(f.severity.clone(), crate::ai_audit::finding_headline(f))
}

/// Rate a style linter's row low: a style linter's severity is a strictness preference, not a
/// judgement about harm. Its security checks (an `eslint-plugin-security` id, a Ruff `S` rule)
/// keep the tool's own severity.
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

/// One row per (repo, tool, tool rule). A row with no tool, or one whose snippet failed the
/// evidence check, stays its own row.
pub(crate) fn group_tool_rows(rows: Vec<Finding>) -> Vec<Finding> {
    let mut order: Vec<Vec<Finding>> = Vec::new();
    let mut index: HashMap<(String, String, String), usize> = HashMap::new();
    for mut row in rows {
        rate_style_lint_low(&mut row);
        let groupable = row.preview_tool.is_some()
            && !row
                .captures
                .contains_key(crate::onboard::evidence_check::EVIDENCE_UNVERIFIED_CAPTURE);
        if !groupable {
            order.push(vec![row]);
            continue;
        }
        let key = (
            row.repo.clone(),
            row.preview_tool.clone().unwrap_or_default(),
            row.rule_id.clone(),
        );
        match index.get(&key) {
            Some(&i) => order[i].push(row),
            None => {
                index.insert(key, order.len());
                order.push(vec![row]);
            }
        }
    }
    order
        .into_iter()
        .map(|mut members| {
            members.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
            let mut it = members.into_iter();
            let mut head = it.next().expect("a group has a member");
            let rest: Vec<Finding> = it.collect();
            if !rest.is_empty() {
                head.also_locations.extend(rest.iter().map(place_of));
                for r in &rest {
                    head.flagged_by.push(crate::flagged_by_of(r));
                }
                if let Some(tool) = head.preview_tool.clone() {
                    crate::note_also_seen_by(&mut head, &tool);
                }
            }
            head
        })
        .collect()
}

/// Fold each grouped tool row into an agent row of the same rule family in the same repository.
/// Returns the tool rows that found no agent row; those stay their own rows.
pub(crate) fn fold_into_agent_rows(
    existing: &mut [Finding],
    groups: Vec<Finding>,
    selectors: Option<&HashMap<String, Vec<String>>>,
) -> Vec<Finding> {
    let tools = crate::review_tree::ToolMap::from_selectors(selectors);
    let mut left = Vec::with_capacity(groups.len());
    for head in groups {
        let family = crate::review_tree::family_of(&head, &tools);
        let target = existing.iter().position(|a| {
            a.captures
                .contains_key(crate::ai_audit::agent_finder::reply::FINDER_PROBLEM_CAPTURE)
                && a.repo == head.repo
                && (crate::review_tree::family_of(a, &tools) == family
                    || crate::preview_is_native_rule_tool(&head.rule_id, &a.rule_id, selectors))
        });
        let Some(i) = target else {
            left.push(head);
            continue;
        };
        let agent = &mut existing[i];
        let mut places = vec![place_of(&head)];
        places.extend(head.also_locations.iter().cloned());
        for p in places {
            let own = p.repo == agent.repo && p.path == agent.path && p.line == agent.line;
            let held = agent.also_locations.iter().any(|l| {
                l.repo == p.repo && l.path == p.path && l.line == p.line && l.rule_id == p.rule_id
            });
            if !own && !held {
                agent.also_locations.push(p);
            }
        }
        if head.rule_id != agent.rule_id && !agent.also_matches.contains(&head.rule_id) {
            agent.also_matches.push(head.rule_id.clone());
        }
        agent.flagged_by.push(crate::flagged_by_of(&head));
        agent.flagged_by.extend(head.flagged_by.iter().cloned());
        // "Tool also found": the tool is named on the row and its places are listed.
        let tool = head.preview_tool.as_deref().unwrap_or("tool");
        crate::note_also_seen_by(agent, tool);
    }
    left
}

#[cfg(test)]
mod tests {
    use super::*;

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
            snippet: "agent snippet".to_string(),
            ..Finding::default()
        };
        f.captures.insert(
            crate::ai_audit::agent_finder::reply::FINDER_PROBLEM_CAPTURE.to_string(),
            "1".to_string(),
        );
        f
    }

    #[test]
    fn twenty_five_rows_of_one_eslint_rule_become_one_low_row_with_all_places() {
        let rows: Vec<Finding> = (0..25)
            .rev()
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
        let out = group_tool_rows(rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, "low");
        assert_eq!(out[0].path, "src/f00.js", "head is the first place by path");
        assert_eq!(out[0].also_locations.len(), 24);
        assert_eq!(out[0].distinct_site_count(), 25);
    }

    #[test]
    fn an_eslint_security_rule_keeps_its_own_severity() {
        let rows = vec![
            tool_row(
                "eslint",
                "security/detect-eval-with-expression",
                "a.js",
                1,
                "high",
            ),
            tool_row(
                "eslint",
                "security/detect-eval-with-expression",
                "b.js",
                2,
                "high",
            ),
        ];
        let out = group_tool_rows(rows);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].severity, "high");
    }

    #[test]
    fn different_tool_rules_stay_separate_rows() {
        let rows = vec![
            tool_row("eslint", "no-unused-vars", "a.js", 1, "low"),
            tool_row("eslint", "no-undef", "a.js", 2, "low"),
        ];
        assert_eq!(group_tool_rows(rows).len(), 2);
    }

    #[test]
    fn a_semgrep_row_in_the_family_of_an_agent_row_folds_into_it() {
        let mut existing = vec![agent_row(
            "SEC-NO-UNSAFE-DESERIALIZATION-1",
            "src/a.rs",
            9,
            "high",
        )];
        let groups = group_tool_rows(vec![
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
        ]);
        let left = fold_into_agent_rows(&mut existing, groups, None);
        assert!(left.is_empty(), "{left:?}");
        assert_eq!(existing.len(), 1);
        assert_eq!(existing[0].also_locations.len(), 2);
        assert_eq!(existing[0].severity, "high");
        assert_eq!(
            existing[0]
                .captures
                .get(crate::ALSO_SEEN_BY_CAPTURE)
                .map(String::as_str),
            Some("semgrep")
        );
        assert!(holds_grouped_places(&existing[0]));
    }

    #[test]
    fn a_camerata_arch_rule_with_no_agent_row_stays_its_own_row_at_its_own_severity() {
        let mut existing = vec![agent_row(
            "SEC-NO-HARDCODED-SECRETS-1",
            "src/a.rs",
            9,
            "high",
        )];
        let groups = group_tool_rows(vec![tool_row(
            crate::onboard::ARCH_PREVIEW_TOOL,
            "ARCH-LAYERING-1",
            "src/a.rs",
            2,
            "high",
        )]);
        let left = fold_into_agent_rows(&mut existing, groups, None);
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].severity, "high");
        assert!(existing[0].also_locations.is_empty());
    }
}
