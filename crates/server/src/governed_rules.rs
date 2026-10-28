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
    /// `inert` names the selected rules that have NO gateway arm: they are carried in the
    /// subset (and so in the agent's grounding) but deny nothing at write time.
    ProjectSelection { selected: usize, inert: Vec<String> },
    /// No project selection: the bundled corpus's Rust/agentic slice, plus the enforced gate
    /// rules.
    CorpusFallback,
}

impl RuleSubsetSource {
    /// One line for the run's events, so the architect can see which rules gated the writes and
    /// which selected rules only ride along.
    pub(crate) fn describe(&self, subset_len: usize) -> String {
        match self {
            Self::ProjectSelection { selected, inert } => {
                let enforced = selected.saturating_sub(inert.len());
                let mut line = format!(
                    "Layer-1 rule subset: the project's {selected} selected rule(s) plus the \
                     enforced gate rules ({subset_len} in total). Enforced at write time: \
                     {enforced} of {selected} selected rule(s) have a gateway arm."
                );
                if !inert.is_empty() {
                    line.push_str(&format!(
                        " Carried but NOT enforced at Layer 1 (no gateway arm, so they deny \
                         nothing at write time): {}.",
                        inert.join(", ")
                    ));
                }
                line
            }
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
    let mut selected: Vec<RuleId> = match project {
        Some(p) => crate::resolve_project_selection_rules(p)
            .await
            .into_iter()
            .map(|r| RuleId(r.id))
            .collect(),
        None => Vec::new(),
    };
    // One rule selected for several placements counts once.
    let mut seen = HashSet::new();
    selected.retain(|id| seen.insert(id.0.clone()));
    if selected.is_empty() {
        log_fallback_once(project.map(|p| p.id.as_str()));
        let role = camerata_fleet::governed_role(role_name).await?;
        return Ok((role, RuleSubsetSource::CorpusFallback));
    }
    let count = selected.len();
    // A selected rule with no gateway arm is carried but denies nothing at write time: name it.
    let inert: Vec<String> = selected
        .iter()
        .filter(|id| camerata_gateway::lookup_arm(&id.0).is_none())
        .map(|id| id.0.clone())
        .collect();
    Ok((
        camerata_fleet::governed_role_from_rules(role_name, selected),
        RuleSubsetSource::ProjectSelection {
            selected: count,
            inert,
        },
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

    /// What the selection does and does not buy at Layer 1. The selected ids are CARRIED in the
    /// subset (and become the run's grounding), but only a rule with a gateway arm denies a
    /// write; the run event must say which of the selected rules are which.
    #[tokio::test]
    async fn a_project_with_sql_and_supabase_rules_carries_them_but_enforces_only_those_with_an_arm(
    ) {
        // Guard the premise: if an arm is added for either inert rule, this test must be
        // revisited rather than silently keep asserting the old split.
        assert!(camerata_gateway::lookup_arm("SEC-NO-RAW-SQL-CONCAT-1").is_some());
        assert!(camerata_gateway::lookup_arm("SQL-PRIVILEGED-WRITE-AUDIT-1").is_none());
        assert!(camerata_gateway::lookup_arm("SUPABASE-RLS-ENABLED-1").is_none());

        let project = project_selecting(SQL_SUPABASE);
        let (role, source) = project_governed_role("BrownfieldImplementer", Some(&project))
            .await
            .unwrap();

        assert_eq!(
            source,
            RuleSubsetSource::ProjectSelection {
                selected: 3,
                inert: vec![
                    "SQL-PRIVILEGED-WRITE-AUDIT-1".to_string(),
                    "SUPABASE-RLS-ENABLED-1".to_string(),
                ],
            }
        );
        for id in SQL_SUPABASE {
            assert!(
                role.rule_subset.contains(&RuleId(id.to_string())),
                "{id} must be carried in the Layer-1 subset: {:?}",
                role.rule_subset
            );
        }
        // The fixed Rust/agentic corpus slice is gone: a Rust rule the project did not select
        // is not carried.
        assert!(!role
            .rule_subset
            .contains(&RuleId("RUST-DOMAIN-2".to_string())));

        // The event line states the split and names the inert rules.
        let line = source.describe(role.rule_subset.len());
        assert!(line.contains("1 of 3"), "{line}");
        assert!(line.contains("SUPABASE-RLS-ENABLED-1"), "{line}");
        assert!(line.contains("SQL-PRIVILEGED-WRITE-AUDIT-1"), "{line}");
        assert!(!line.contains("SEC-NO-RAW-SQL-CONCAT-1 "), "{line}");

        // The inert rule enforces nothing: a table created without row level security is
        // allowed at write time even though SUPABASE-RLS-ENABLED-1 is selected.
        let decision = evaluate_call(
            &role.rule_subset,
            &write("db/schema.sql", "create table accounts (id int);"),
        );
        assert!(matches!(decision, Decision::Allow), "{decision:?}");
    }

    /// The SQL concatenation denial is NOT sourced from the selection: the gate rule is always
    /// on, so a project that selected only an unrelated rule is denied the same write.
    #[tokio::test]
    async fn the_sql_concat_denial_does_not_depend_on_the_selection() {
        let project = project_selecting(&["SUPABASE-RLS-ENABLED-1"]);
        let (role, _) = project_governed_role("BrownfieldImplementer", Some(&project))
            .await
            .unwrap();
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
    async fn a_selection_of_only_enforced_rules_reports_no_inert_rules() {
        let project = project_selecting(&["SEC-NO-RAW-SQL-CONCAT-1"]);
        let (role, source) = project_governed_role("BrownfieldImplementer", Some(&project))
            .await
            .unwrap();
        assert_eq!(
            source,
            RuleSubsetSource::ProjectSelection {
                selected: 1,
                inert: Vec::new(),
            }
        );
        let line = source.describe(role.rule_subset.len());
        assert!(line.contains("1 of 1"), "{line}");
        assert!(!line.contains("NOT enforced"), "{line}");
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
            RuleSubsetSource::ProjectSelection {
                selected: 2,
                inert: vec!["SUPABASE-RLS-ENABLED-1".to_string()],
            },
            RuleSubsetSource::CorpusFallback,
        ] {
            assert!(!s.describe(9).contains('\u{2014}'));
        }
    }
}
