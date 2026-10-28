//! The Layer 1 (write-time) rule subset a console agent runs under.
//!
//! A brownfield agent works in a client's repository, so its gate must carry THAT project's
//! selected rules, not a fixed slice of the bundled corpus. The project's rules are resolved by
//! [`crate::resolve_project_selection_rules`], the same resolver the inspection's delta rescan
//! uses, so "the project's rules" has one definition for both inspecting and writing code.
//!
//! The enforced gate rules ([`camerata_gateway::enforced_gate_rules`]) are ALWAYS in the subset
//! on top of the selection (via [`camerata_fleet::governed_role_from_rules`]): a project that did
//! not select, say, the hardcoded-secret rule still cannot write a secret. A rule that should be
//! enforced must never silently drop out of the gate because a selection omitted it.
//!
//! A project with no selection (or no active project) falls back to the previous subset, the
//! bundled corpus's Rust/agentic slice ([`camerata_fleet::governed_role`]), and logs that once
//! per project per process.

use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

use camerata_core::{Role, RuleId};

/// Where a role's rule subset came from. Reported in the run's events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuleSubsetSource {
    /// The project's selected rules (`selected` of them), plus the enforced gate rules.
    ProjectSelection { selected: usize },
    /// No project selection: the bundled corpus's Rust/agentic slice, plus the enforced gate
    /// rules.
    CorpusFallback,
}

impl RuleSubsetSource {
    /// One line for the run's events, so the architect can see which rules gated the writes.
    pub(crate) fn describe(&self, subset_len: usize) -> String {
        match self {
            Self::ProjectSelection { selected } => format!(
                "Layer-1 rule subset: the project's {selected} selected rule(s) plus the \
                 enforced gate rules ({subset_len} in total)."
            ),
            Self::CorpusFallback => format!(
                "Layer-1 rule subset: this project has no rule selection, so the built-in \
                 Rust/agentic corpus subset applies, plus the enforced gate rules \
                 ({subset_len} in total). Select the project's rules to gate writes by them."
            ),
        }
    }
}

/// Build the governed role `role_name` for an agent working on `project`: the project's
/// selected rules plus the enforced gate rules, or the corpus fallback when there is no
/// selection. Errors only on the fallback path (the corpus failed to load).
pub(crate) async fn project_governed_role(
    role_name: &str,
    project: Option<&crate::project::Project>,
) -> anyhow::Result<(Role, RuleSubsetSource)> {
    let selected: Vec<RuleId> = match project {
        Some(p) => crate::resolve_project_selection_rules(p)
            .await
            .into_iter()
            .map(|r| RuleId(r.id))
            .collect(),
        None => Vec::new(),
    };
    if selected.is_empty() {
        log_fallback_once(project.map(|p| p.id.as_str()));
        let role = camerata_fleet::governed_role(role_name).await?;
        return Ok((role, RuleSubsetSource::CorpusFallback));
    }
    let count = selected.len();
    Ok((
        camerata_fleet::governed_role_from_rules(role_name, selected),
        RuleSubsetSource::ProjectSelection { selected: count },
    ))
}

/// Log the corpus fallback once per project (or once for "no active project") per process,
/// so a project without a selection is visible in the log without repeating on every run.
/// Returns whether this call logged.
fn log_fallback_once(project_id: Option<&str>) -> bool {
    static LOGGED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let key = project_id.unwrap_or("<no active project>").to_string();
    let first = LOGGED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key.clone());
    if first {
        tracing::warn!(
            project = %key,
            "no project rule selection: the agent's Layer-1 subset falls back to the built-in \
             Rust/agentic corpus slice plus the enforced gate rules"
        );
    }
    first
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_core::{Decision, ToolCall};
    use camerata_gateway::{enforced_gate_rules, evaluate_call};

    fn project_selecting(ids: &[&str]) -> crate::project::Project {
        let store = crate::project::ProjectStore::default();
        let p = store.create("Acme", vec!["acme/db".to_string()]).unwrap();
        store
            .update(&p.id, |p| {
                for id in ids {
                    p.ruleset.selections.push(crate::project::RuleSelection {
                        rule_id: id.to_string(),
                        ..Default::default()
                    });
                }
            })
            .unwrap()
    }

    fn write(path: &str, content: &str) -> ToolCall {
        ToolCall {
            tool: "gated_write".to_string(),
            input: serde_json::json!({ "path": path, "content": content }),
        }
    }

    const SQL_SUPABASE: &[&str] = &[
        "SEC-NO-RAW-SQL-CONCAT-1",
        "SQL-PRIVILEGED-WRITE-AUDIT-1",
        "SUPABASE-RLS-ENABLED-1",
    ];

    #[tokio::test]
    async fn a_project_with_sql_and_supabase_rules_selected_gates_writes_by_them() {
        let project = project_selecting(SQL_SUPABASE);

        let (role, source) = project_governed_role("BrownfieldImplementer", Some(&project))
            .await
            .unwrap();

        assert_eq!(source, RuleSubsetSource::ProjectSelection { selected: 3 });
        for id in SQL_SUPABASE {
            assert!(
                role.rule_subset.contains(&RuleId(id.to_string())),
                "{id} must be in the Layer-1 subset: {:?}",
                role.rule_subset
            );
        }
        // The fixed Rust/agentic corpus slice is gone: a Rust rule the project did not select
        // is not carried.
        assert!(!role
            .rule_subset
            .contains(&RuleId("RUST-DOMAIN-2".to_string())));
        // And the selected SQL rule is ENFORCED at write time by the gate.
        let decision = evaluate_call(
            &role.rule_subset,
            &write(
                "db/queries.ts",
                "const q = \"SELECT * FROM users WHERE id = \" + userId;",
            ),
        );
        match decision {
            Decision::Deny { rule, .. } => assert_eq!(rule.0, "SEC-NO-RAW-SQL-CONCAT-1"),
            Decision::Allow => panic!("an SQL string concatenation must be denied at write time"),
        }
    }

    #[tokio::test]
    async fn the_enforced_gate_rules_stay_on_even_when_the_selection_omits_them() {
        let project = project_selecting(&["SUPABASE-RLS-ENABLED-1"]);
        let (role, _) = project_governed_role("BrownfieldImplementer", Some(&project))
            .await
            .unwrap();
        for gate_rule in enforced_gate_rules() {
            assert!(
                role.rule_subset.contains(&gate_rule),
                "{gate_rule:?} dropped"
            );
        }
        let decision = evaluate_call(
            &role.rule_subset,
            &write("src/config.ts", "const key = \"AKIAABCDEFGHIJKLMNOP\";"),
        );
        assert!(matches!(decision, Decision::Deny { .. }), "{decision:?}");
    }

    #[tokio::test]
    async fn no_selection_falls_back_to_the_previous_corpus_subset() {
        let project = project_selecting(&[]);
        let (role, source) = project_governed_role("BrownfieldImplementer", Some(&project))
            .await
            .unwrap();
        let today = camerata_fleet::governed_role("BrownfieldImplementer")
            .await
            .unwrap();
        assert_eq!(source, RuleSubsetSource::CorpusFallback);
        assert_eq!(role.rule_subset, today.rule_subset);

        let (role, source) = project_governed_role("BrownfieldImplementer", None)
            .await
            .unwrap();
        assert_eq!(source, RuleSubsetSource::CorpusFallback);
        assert_eq!(role.rule_subset, today.rule_subset);
    }

    #[test]
    fn the_fallback_is_logged_once_per_project() {
        let id = "governed-rules-log-once-test-project";
        assert!(log_fallback_once(Some(id)));
        assert!(!log_fallback_once(Some(id)));
    }

    #[test]
    fn the_source_lines_carry_no_em_dash() {
        for s in [
            RuleSubsetSource::ProjectSelection { selected: 2 },
            RuleSubsetSource::CorpusFallback,
        ] {
            assert!(!s.describe(9).contains('\u{2014}'));
        }
    }
}
