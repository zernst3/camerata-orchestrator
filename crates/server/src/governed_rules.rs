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
    ProjectSelection {
        selected: usize,
        inert: Vec<String>,
        /// Selected rules bound to OTHER repositories of the project (their `repos` do not
        /// include this run's repository), so they are not applied here.
        other_repos: usize,
        /// Rules the unit's own findings add on top of the selection (a unit created from an
        /// inspection's findings, roadmap 2c). A rule both selected and a focus rule counts once,
        /// in `selected`. Zero for every other unit.
        focus: usize,
        /// How many of the `focus` rules have a gateway arm and so deny at write time.
        focus_enforced: usize,
    },
    /// No project selection: the bundled corpus's Rust/agentic slice, plus the enforced gate
    /// rules, plus the unit's focus rules (see `focus` above).
    CorpusFallback { focus: usize, focus_enforced: usize },
}

impl RuleSubsetSource {
    /// One line for the run's events, so the architect can see which rules gated the writes and
    /// which selected rules only ride along.
    pub(crate) fn describe(&self, subset_len: usize) -> String {
        match self {
            Self::ProjectSelection {
                selected,
                inert,
                other_repos,
                focus,
                focus_enforced,
            } => {
                let enforced = selected.saturating_sub(inert.len());
                let mut line = if *selected == 0 && *other_repos > 0 {
                    format!(
                        "Layer-1 rule subset: none of the project's selected rules apply to \
                         this repository, so only the enforced gate rules gate writes \
                         ({subset_len} in total)."
                    )
                } else {
                    format!(
                        "Layer-1 rule subset: the project's {selected} selected rule(s) plus \
                         the enforced gate rules ({subset_len} in total). Enforced at write \
                         time: {enforced} of {selected} selected rule(s) have a gateway arm."
                    )
                };
                if !inert.is_empty() {
                    line.push_str(&format!(
                        " Carried but NOT enforced at Layer 1 (no gateway arm, so they deny \
                         nothing at write time): {}.",
                        inert.join(", ")
                    ));
                }
                if *other_repos > 0 {
                    line.push_str(&format!(
                        " {other_repos} selected rule(s) are bound to other repositories and \
                         are not applied here."
                    ));
                }
                line.push_str(&focus_sentence(*focus, *focus_enforced));
                line
            }
            Self::CorpusFallback {
                focus,
                focus_enforced,
            } => format!(
                "Layer-1 rule subset: this project has no rule selection, so the built-in \
                 Rust/agentic corpus subset applies, plus the enforced gate rules \
                 ({subset_len} in total). Select the project's rules to gate writes by them.{}",
                focus_sentence(*focus, *focus_enforced)
            ),
        }
    }
}

/// The sentence that names the rules a unit's findings added, or nothing when there are none.
/// A focus rule without a gateway arm enforces nothing at Layer 1; the sentence says so.
fn focus_sentence(focus: usize, focus_enforced: usize) -> String {
    if focus == 0 {
        return String::new();
    }
    format!(
        " {focus} rule(s) come from the unit's findings; {focus_enforced} of them have a gateway \
         arm and deny at write time, the rest are carried as grounding."
    )
}

/// Build the governed role `role_name` for an agent working on `project`: the project's
/// selected rules plus the enforced gate rules, or the corpus fallback when there is no
/// selection. Errors only on the fallback path (the corpus failed to load).
///
/// `focus_rules` are the rule ids of the unit's own findings (empty for a unit that did not come
/// from findings): they are appended after the selection, so a unit that must fix a rule the
/// project did not select still carries it. The enforced gate rules stay first either way.
pub(crate) async fn project_governed_role(
    role_name: &str,
    project: Option<&crate::project::Project>,
    repo: Option<&str>,
    focus_rules: &[String],
) -> anyhow::Result<(Role, RuleSubsetSource)> {
    // The project's selections, resolved by the same resolver the inspection uses. A
    // selection's `repos` binds it to repositories (empty = project-level, applies to every
    // repo), exactly as the inspection's `project_scan_request` hands them to the scan; the
    // subset therefore holds only the rules that apply to the repository this run works in.
    let resolved = match project {
        Some(p) => crate::resolve_project_selection_rules(p).await,
        None => Vec::new(),
    };
    let mut selected: Vec<RuleId> = Vec::new();
    let mut seen = HashSet::new();
    let mut other_repos_ids = HashSet::new();
    for rule in resolved.iter() {
        let applies = match repo {
            Some(repo) => rule.repos.is_empty() || rule.repos.iter().any(|r| r == repo),
            None => true,
        };
        if !applies {
            other_repos_ids.insert(rule.id.clone());
        } else if seen.insert(rule.id.clone()) {
            selected.push(RuleId(rule.id.clone()));
        }
    }
    // A rule bound to another repo in one placement and to this one in another still applies.
    other_repos_ids.retain(|id| !seen.contains(id));
    let other_repos = other_repos_ids.len();
    // The unit's own rules, beyond what the selection already carries.
    let mut focus_added: Vec<RuleId> = Vec::new();
    for id in focus_rules
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
    {
        let id = RuleId(id.to_string());
        if !seen.contains(&id.0) && !focus_added.contains(&id) {
            focus_added.push(id);
        }
    }
    let focus = focus_added.len();
    let focus_enforced = focus_added
        .iter()
        .filter(|id| camerata_gateway::lookup_arm(&id.0).is_some())
        .count();
    if resolved.is_empty() {
        log_fallback_once(project.map(|p| p.id.as_str()));
        let mut role = camerata_fleet::governed_role(role_name).await?;
        for id in focus_added {
            if !role.rule_subset.contains(&id) {
                role.rule_subset.push(id);
            }
        }
        return Ok((
            role,
            RuleSubsetSource::CorpusFallback {
                focus,
                focus_enforced,
            },
        ));
    }
    let count = selected.len();
    // A selected rule with no gateway arm is carried but denies nothing at write time: name it.
    let inert: Vec<String> = selected
        .iter()
        .filter(|id| camerata_gateway::lookup_arm(&id.0).is_none())
        .map(|id| id.0.clone())
        .collect();
    selected.extend(focus_added);
    Ok((
        camerata_fleet::governed_role_from_rules(role_name, selected),
        RuleSubsetSource::ProjectSelection {
            selected: count,
            inert,
            other_repos,
            focus,
            focus_enforced,
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
        let (role, source) =
            project_governed_role("BrownfieldImplementer", Some(&project), None, &[])
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
                other_repos: 0,
                focus: 0,
                focus_enforced: 0,
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
    /// A project of two repos where each selection is bound to a repo (or to none).
    fn project_with_scoped_selections(selections: &[(&str, &[&str])]) -> crate::project::Project {
        let store = crate::project::ProjectStore::default();
        let p = store
            .create("Acme", vec!["acme/db".to_string(), "acme/web".to_string()])
            .unwrap();
        store
            .update(&p.id, |p| {
                for (id, repos) in selections {
                    p.ruleset.selections.push(crate::project::RuleSelection {
                        rule_id: id.to_string(),
                        repos: repos.iter().map(|r| r.to_string()).collect(),
                        ..Default::default()
                    });
                }
            })
            .unwrap()
    }

    fn carries(role: &Role, id: &str) -> bool {
        role.rule_subset.contains(&RuleId(id.to_string()))
    }

    /// The governed subset honours a selection's `repos` binding exactly as the inspection
    /// does (`project_scan_request` passes each rule's repos): a rule bound to another repo is
    /// not applied here, a rule with no binding (project-level) applies everywhere.
    #[tokio::test]
    async fn selections_bound_to_other_repos_are_not_in_this_repos_subset() {
        let project = project_with_scoped_selections(&[
            ("SUPABASE-RLS-ENABLED-1", &["acme/db"]),
            ("SQL-PRIVILEGED-WRITE-AUDIT-1", &["acme/web"]),
            ("SEC-NO-RAW-SQL-CONCAT-1", &[]),
        ]);

        let (db, db_source) = project_governed_role(
            "BrownfieldImplementer",
            Some(&project),
            Some("acme/db"),
            &[],
        )
        .await
        .unwrap();
        assert!(carries(&db, "SUPABASE-RLS-ENABLED-1"));
        assert!(carries(&db, "SEC-NO-RAW-SQL-CONCAT-1"));
        assert!(!carries(&db, "SQL-PRIVILEGED-WRITE-AUDIT-1"));
        assert_eq!(
            db_source,
            RuleSubsetSource::ProjectSelection {
                selected: 2,
                inert: vec!["SUPABASE-RLS-ENABLED-1".to_string()],
                other_repos: 1,
                focus: 0,
                focus_enforced: 0,
            }
        );
        assert!(
            db_source
                .describe(db.rule_subset.len())
                .contains("1 selected rule(s) are bound to other repositories"),
            "{}",
            db_source.describe(db.rule_subset.len())
        );

        let (web, _) = project_governed_role(
            "BrownfieldImplementer",
            Some(&project),
            Some("acme/web"),
            &[],
        )
        .await
        .unwrap();
        assert!(carries(&web, "SQL-PRIVILEGED-WRITE-AUDIT-1"));
        assert!(carries(&web, "SEC-NO-RAW-SQL-CONCAT-1"));
        assert!(!carries(&web, "SUPABASE-RLS-ENABLED-1"));

        // With no repo to scope by, every selection applies.
        let (all, source) =
            project_governed_role("BrownfieldImplementer", Some(&project), None, &[])
                .await
                .unwrap();
        assert!(carries(&all, "SUPABASE-RLS-ENABLED-1"));
        assert!(carries(&all, "SQL-PRIVILEGED-WRITE-AUDIT-1"));
        assert!(matches!(
            source,
            RuleSubsetSource::ProjectSelection {
                selected: 3,
                other_repos: 0,
                focus: 0,
                focus_enforced: 0,
                ..
            }
        ));
    }

    /// A project whose selections are all bound to OTHER repos still has a selection: it must
    /// not be reported as "no rule selection" and fall back to the Rust/agentic corpus slice.
    #[tokio::test]
    async fn a_repo_that_none_of_the_selections_bind_to_gets_only_the_gate_rules() {
        let project = project_with_scoped_selections(&[("SUPABASE-RLS-ENABLED-1", &["acme/db"])]);

        let (role, source) = project_governed_role(
            "BrownfieldImplementer",
            Some(&project),
            Some("acme/web"),
            &[],
        )
        .await
        .unwrap();

        assert_eq!(
            source,
            RuleSubsetSource::ProjectSelection {
                selected: 0,
                inert: Vec::new(),
                other_repos: 1,
                focus: 0,
                focus_enforced: 0,
            }
        );
        assert!(!carries(&role, "SUPABASE-RLS-ENABLED-1"));
        assert!(!carries(&role, "RUST-DOMAIN-2"));
        for gate_rule in enforced_gate_rules() {
            assert!(role.rule_subset.contains(&gate_rule), "{gate_rule:?}");
        }
        let line = source.describe(role.rule_subset.len());
        assert!(
            line.contains("none of the project's selected rules"),
            "{line}"
        );
    }

    #[tokio::test]
    async fn the_sql_concat_denial_does_not_depend_on_the_selection() {
        let project = project_selecting(&["SUPABASE-RLS-ENABLED-1"]);
        let (role, _) = project_governed_role("BrownfieldImplementer", Some(&project), None, &[])
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
        let (role, source) =
            project_governed_role("BrownfieldImplementer", Some(&project), None, &[])
                .await
                .unwrap();
        assert_eq!(
            source,
            RuleSubsetSource::ProjectSelection {
                selected: 1,
                inert: Vec::new(),
                other_repos: 0,
                focus: 0,
                focus_enforced: 0,
            }
        );
        let line = source.describe(role.rule_subset.len());
        assert!(line.contains("1 of 1"), "{line}");
        assert!(!line.contains("NOT enforced"), "{line}");
    }

    #[tokio::test]
    async fn the_enforced_gate_rules_stay_on_even_when_the_selection_omits_them() {
        let project = project_selecting(&["SUPABASE-RLS-ENABLED-1"]);
        let (role, _) = project_governed_role("BrownfieldImplementer", Some(&project), None, &[])
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
        let (role, source) =
            project_governed_role("BrownfieldImplementer", Some(&project), None, &[])
                .await
                .unwrap();
        let today = camerata_fleet::governed_role("BrownfieldImplementer")
            .await
            .unwrap();
        assert_eq!(
            source,
            RuleSubsetSource::CorpusFallback {
                focus: 0,
                focus_enforced: 0
            }
        );
        assert_eq!(role.rule_subset, today.rule_subset);

        let (role, source) = project_governed_role("BrownfieldImplementer", None, None, &[])
            .await
            .unwrap();
        assert_eq!(
            source,
            RuleSubsetSource::CorpusFallback {
                focus: 0,
                focus_enforced: 0
            }
        );
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
                other_repos: 1,
                focus: 0,
                focus_enforced: 0,
            },
            RuleSubsetSource::CorpusFallback {
                focus: 0,
                focus_enforced: 0,
            },
            RuleSubsetSource::ProjectSelection {
                selected: 2,
                inert: vec![],
                other_repos: 0,
                focus: 2,
                focus_enforced: 1,
            },
        ] {
            assert!(!s.describe(9).contains('\u{2014}'));
        }
    }

    /// A unit created from inspection findings must satisfy the rules its findings violate, even
    /// when the project did not select them. The focus rules join the Layer-1 subset, are counted
    /// in the run's event, and the event is honest about which of them deny at write time.
    #[tokio::test]
    async fn a_unit_from_findings_runs_with_its_rules_in_the_layer_1_subset() {
        // Premise: the focus rule is neither selected nor a gate rule, and has no gateway arm.
        assert!(camerata_gateway::lookup_arm("SQL-PRIVILEGED-WRITE-AUDIT-1").is_none());
        let project = project_selecting(&["SUPABASE-RLS-ENABLED-1"]);
        let focus = vec!["SQL-PRIVILEGED-WRITE-AUDIT-1".to_string()];

        let (without, _) = project_governed_role(
            "BrownfieldImplementer",
            Some(&project),
            Some("acme/db"),
            &[],
        )
        .await
        .unwrap();
        assert!(!without
            .rule_subset
            .contains(&RuleId("SQL-PRIVILEGED-WRITE-AUDIT-1".to_string())));

        let (role, source) = project_governed_role(
            "BrownfieldImplementer",
            Some(&project),
            Some("acme/db"),
            &focus,
        )
        .await
        .unwrap();
        assert!(role
            .rule_subset
            .contains(&RuleId("SQL-PRIVILEGED-WRITE-AUDIT-1".to_string())));
        assert!(role
            .rule_subset
            .contains(&RuleId("SUPABASE-RLS-ENABLED-1".to_string())));
        let RuleSubsetSource::ProjectSelection {
            selected,
            focus,
            focus_enforced,
            ..
        } = source.clone()
        else {
            panic!("a project with a selection: {source:?}");
        };
        assert_eq!((selected, focus, focus_enforced), (1, 1, 0));
        let line = source.describe(role.rule_subset.len());
        assert!(
            line.contains(
                "1 rule(s) come from the unit's findings; 0 of them have a gateway arm and \
                 deny at write time, the rest are carried as grounding."
            ),
            "{line}"
        );
    }

    /// A rule both selected and a focus rule counts once; a blank or repeated focus id adds
    /// nothing.
    #[tokio::test]
    async fn a_focus_rule_that_is_also_selected_counts_once() {
        let project = project_selecting(&["SUPABASE-RLS-ENABLED-1"]);
        let focus = vec![
            "SUPABASE-RLS-ENABLED-1".to_string(),
            " ".to_string(),
            "SQL-PRIVILEGED-WRITE-AUDIT-1".to_string(),
            "SQL-PRIVILEGED-WRITE-AUDIT-1".to_string(),
        ];
        let (role, source) = project_governed_role(
            "BrownfieldImplementer",
            Some(&project),
            Some("acme/db"),
            &focus,
        )
        .await
        .unwrap();
        let count = |id: &str| role.rule_subset.iter().filter(|r| r.0 == id).count();
        assert_eq!(count("SUPABASE-RLS-ENABLED-1"), 1);
        assert_eq!(count("SQL-PRIVILEGED-WRITE-AUDIT-1"), 1);
        let RuleSubsetSource::ProjectSelection { focus, .. } = source else {
            panic!("a selection");
        };
        assert_eq!(
            focus, 1,
            "only the rule the selection lacked is a focus addition"
        );
    }

    /// With no project selection the unit still gates on its own rules, on top of the corpus
    /// fallback.
    #[tokio::test]
    async fn the_fallback_path_also_carries_the_focus_rules() {
        let (role, source) = project_governed_role(
            "BrownfieldImplementer",
            None,
            None,
            &["SQL-PRIVILEGED-WRITE-AUDIT-1".to_string()],
        )
        .await
        .unwrap();
        assert!(role
            .rule_subset
            .contains(&RuleId("SQL-PRIVILEGED-WRITE-AUDIT-1".to_string())));
        assert_eq!(
            source,
            RuleSubsetSource::CorpusFallback {
                focus: 1,
                focus_enforced: 0
            }
        );
        assert!(source
            .describe(role.rule_subset.len())
            .contains("1 rule(s) come from"));
    }
}
