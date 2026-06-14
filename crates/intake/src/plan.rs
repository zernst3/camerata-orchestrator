//! The lead engineer's structured output: a buildable [`Plan`].
//!
//! A `Plan` is the PO-mode analogue of the architect-approved plan in the
//! cockpit. It is intentionally the same SHAPE the governed fleet already
//! consumes: an ordered list of role+task stages. PO mode's only job is to turn
//! a story-level [`crate::form::IntakeForm`] into this engineering plan; the
//! governed fleet (`camerata-core`) takes it from there, unchanged.

use serde::{Deserialize, Serialize};

/// Which layer of the bespoke app a [`PlanTask`] builds. This is provenance /
/// routing metadata: it tells the orchestrator which kind of role should run the
/// task (and, later, which path-scope + rule-subset to deliver). V1's `po-demo`
/// maps these onto governed fleet stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Persistence: schema, migrations, repository layer.
    Database,
    /// Backend: domain types, services, the API surface.
    Backend,
    /// Frontend: the views/screens over the entities.
    Frontend,
    /// Verification: tests over the produced code.
    Test,
}

impl TaskKind {
    /// A human label for the task kind.
    pub fn label(&self) -> &'static str {
        match self {
            TaskKind::Database => "database",
            TaskKind::Backend => "backend",
            TaskKind::Frontend => "frontend",
            TaskKind::Test => "test",
        }
    }
}

/// One unit of work in a [`Plan`]: a role name, the layer it belongs to, and a
/// precise task description. The description is what a governed agent is handed;
/// keeping it precise is what makes a first-try governed build plausible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanTask {
    /// The role that should run this task (e.g. `Implementer`, `Tester`). This
    /// becomes the governed fleet stage's role name.
    pub role: String,
    /// Which app layer this task builds.
    pub kind: TaskKind,
    /// The precise instruction for the agent running this task.
    pub description: String,
}

/// The lead engineer's plan for one bespoke app: a summary plus an ORDERED list
/// of tasks. Order is the build order the governed fleet runs in (an earlier
/// task's worktree output is visible to a later one).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    /// The app name (echoed from the form, normalized by the lead engineer).
    pub app_name: String,
    /// A one-paragraph engineering summary of what will be built.
    pub summary: String,
    /// The ordered build tasks.
    pub tasks: Vec<PlanTask>,
}

impl Plan {
    /// Whether the plan has at least one task (a usable plan).
    pub fn is_buildable(&self) -> bool {
        !self.tasks.is_empty()
    }

    /// The number of tasks in the plan.
    pub fn task_count(&self) -> usize {
        self.tasks.len()
    }

    /// Render the plan as a compact, human-readable block for the demo summary.
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("Plan for: {}\n", self.app_name));
        out.push_str(&format!("Summary:  {}\n", self.summary));
        out.push_str(&format!("Tasks ({}):\n", self.tasks.len()));
        for (i, task) in self.tasks.iter().enumerate() {
            out.push_str(&format!(
                "  {}. [{}/{}] {}\n",
                i + 1,
                task.kind.label(),
                task.role,
                task.description,
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Plan {
        Plan {
            app_name: "budget-tracker".to_string(),
            summary: "Expense CRUD + list".to_string(),
            tasks: vec![
                PlanTask {
                    role: "Implementer".to_string(),
                    kind: TaskKind::Backend,
                    description: "the Expense type".to_string(),
                },
                PlanTask {
                    role: "Tester".to_string(),
                    kind: TaskKind::Test,
                    description: "test it".to_string(),
                },
            ],
        }
    }

    #[test]
    fn buildable_when_it_has_tasks() {
        assert!(plan().is_buildable());
        assert_eq!(plan().task_count(), 2);
    }

    #[test]
    fn empty_plan_is_not_buildable() {
        let empty = Plan {
            app_name: "x".to_string(),
            summary: "y".to_string(),
            tasks: vec![],
        };
        assert!(!empty.is_buildable());
    }

    #[test]
    fn render_lists_each_task_with_kind_and_role() {
        let r = plan().render();
        assert!(r.contains("budget-tracker"));
        assert!(r.contains("[backend/Implementer]"));
        assert!(r.contains("[test/Tester]"));
    }

    #[test]
    fn plan_roundtrips_through_json() {
        let p = plan();
        let json = serde_json::to_string(&p).unwrap();
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
