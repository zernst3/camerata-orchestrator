//! The lead-engineer seam: evaluate a Product-Owner [`IntakeForm`] and emit an
//! [`Intake`] (a [`Plan`] when ready, or clarifying questions).
//!
//! Two implementations ship:
//!
//! - [`ClaudeLeadEngineer`] — the REAL evaluation. It spawns a headless
//!   `claude -p` (no governance gate: this is a planning/architecture call, not a
//!   worktree write), asks for a strict-JSON plan over the form, and parses it.
//!   The governance gate is for the BUILDERS; the lead engineer only *plans*.
//! - [`StubLeadEngineer`] — a deterministic, no-network fallback that derives a
//!   plan straight from the form's shape. Used in tests and as the `po-demo`
//!   fallback when the live call fails, so the pipeline always has SOMETHING to
//!   hand the governed fleet.
//!
//! Provider / model / tier live BEHIND this seam (same stance as
//! [`camerata_core::AgentDriver`]). Core never names a model; PO mode names one
//! only in the concrete [`ClaudeLeadEngineer`].

use async_trait::async_trait;
use thiserror::Error;

use crate::form::IntakeForm;
use crate::plan::{Plan, PlanTask, TaskKind};
use crate::Intake;

/// Errors from running a lead engineer over a form (RUST-DOMAIN-4 / -6).
#[derive(Debug, Error)]
pub enum LeadEngineerError {
    /// The `claude` process could not be spawned.
    #[error("failed to spawn `claude`: {0}")]
    Spawn(#[source] std::io::Error),

    /// `claude -p` exited non-zero.
    #[error("`claude -p` exited with status {status}: {stderr}")]
    NonZeroExit { status: String, stderr: String },

    /// The CLI's outer JSON envelope did not parse.
    #[error("could not parse `claude -p` JSON envelope: {0}")]
    ParseEnvelope(#[source] serde_json::Error),

    /// The model's inner `result` text was not the JSON plan we asked for.
    #[error("lead engineer did not return a parseable plan: {0}")]
    ParsePlan(String),
}

/// LEAD-ENGINEER SEAM — evaluate a PO form and produce an [`Intake`].
///
/// This is the PO-mode entry point: it is to the intake form what the
/// investigation agent is to a Story in architect mode. It returns an [`Intake`]
/// so a future multi-turn clarify loop can branch on
/// [`Intake::NeedsClarification`]; V1 drives the [`Intake::Ready`] arm.
#[async_trait]
pub trait LeadEngineer: Send + Sync {
    /// Evaluate `form` and return either a buildable plan or clarifying
    /// questions.
    async fn evaluate(&self, form: &IntakeForm) -> Result<Intake, LeadEngineerError>;
}

// ─── deterministic fallback ──────────────────────────────────────────────────

/// A deterministic, no-network lead engineer.
///
/// It derives a plan directly from the form's shape: one backend task (the
/// entity types) plus one test task. It never asks clarifying questions — it is
/// the fallback that guarantees the PO pipeline always has a plan to build, even
/// when the live model call is unavailable. This is honest: when `po-demo`
/// reports it fell back to the stub, that is a real signal that the live
/// evaluation did not happen, not a faked success.
#[derive(Debug, Default, Clone)]
pub struct StubLeadEngineer;

impl StubLeadEngineer {
    /// Construct the stub lead engineer.
    pub fn new() -> Self {
        Self
    }

    /// The deterministic plan derived from a form. Public so `po-demo` can build
    /// the exact same fleet tasks whether the plan came from the model or the
    /// stub (the governed fleet consumes a `Plan`, not a `LeadEngineer`).
    pub fn plan_for(form: &IntakeForm) -> Plan {
        let entity_names: Vec<&str> = form.entities.iter().map(|e| e.name.as_str()).collect();
        let entities_joined = entity_names.join(", ");

        let backend = PlanTask {
            role: "Implementer".to_string(),
            kind: TaskKind::Backend,
            description: format!(
                "Define the core domain types for the {app} app: {entities}. \
                 For each entity, a public Rust struct with the listed fields. \
                 Plain library Rust, no tests.",
                app = form.app_name,
                entities = entities_joined,
            ),
        };
        let test = PlanTask {
            role: "Tester".to_string(),
            kind: TaskKind::Test,
            description: format!(
                "Add a `#[cfg(test)]` module that constructs each of \
                 [{entities}] and asserts the fields round-trip.",
                entities = entities_joined,
            ),
        };

        Plan {
            app_name: form.app_name.clone(),
            summary: format!(
                "Bespoke CRUD app '{app}': {n} entity(ies) ({entities}) with \
                 {v} view(s). Backend domain types first, then tests.",
                app = form.app_name,
                n = form.entities.len(),
                entities = entities_joined,
                v = form.views.len(),
            ),
            tasks: vec![backend, test],
        }
    }
}

#[async_trait]
impl LeadEngineer for StubLeadEngineer {
    async fn evaluate(&self, form: &IntakeForm) -> Result<Intake, LeadEngineerError> {
        Ok(Intake::Ready(Self::plan_for(form)))
    }
}

// ─── the real Claude lead engineer ───────────────────────────────────────────

/// The default model id the live lead-engineer call uses. Behind the seam, so
/// only this concrete type names a model.
pub const DEFAULT_LEAD_ENGINEER_MODEL: &str = "claude-sonnet-4-5";

/// The REAL lead engineer: a headless `claude -p` call that evaluates the form
/// as a lead engineer and returns a strict-JSON plan.
///
/// No governance gate is involved — this is a planning call that writes nothing
/// to the worktree. (The gate governs the BUILDERS, which run later in the
/// governed fleet.) The call is constrained to be read-only and JSON-only.
#[derive(Debug, Clone)]
pub struct ClaudeLeadEngineer {
    model: String,
}

impl Default for ClaudeLeadEngineer {
    fn default() -> Self {
        Self::new()
    }
}

impl ClaudeLeadEngineer {
    /// Construct with the default model.
    pub fn new() -> Self {
        Self {
            model: DEFAULT_LEAD_ENGINEER_MODEL.to_string(),
        }
    }

    /// Construct with an explicit model id.
    pub fn with_model(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
        }
    }

    /// Build the prompt that asks the model to evaluate the form and emit a plan.
    ///
    /// Pure + public so it is unit-testable without spawning a process, and so
    /// the demo can show exactly what the lead engineer was asked.
    pub fn build_prompt(form: &IntakeForm) -> String {
        format!(
            "You are the LEAD ENGINEER evaluating a Product Owner's intake form \
             for a small bespoke CRUD app. Read the brief, then output a build \
             plan.\n\n\
             === INTAKE FORM ===\n{brief}\n\
             === END FORM ===\n\n\
             Output ONLY a single JSON object (no prose, no markdown fences) with \
             this exact shape:\n\
             {{\n\
             \x20 \"app_name\": string,\n\
             \x20 \"summary\": string,            // one-paragraph engineering summary\n\
             \x20 \"tasks\": [                      // ordered build steps\n\
             \x20   {{ \"role\": string,            // e.g. \"Implementer\", \"Tester\"\n\
             \x20      \"kind\": \"database\"|\"backend\"|\"frontend\"|\"test\",\n\
             \x20      \"description\": string }}   // precise instruction for one governed agent\n\
             \x20 ]\n\
             }}\n\n\
             Keep the plan small and CRUD-shaped: a backend task that defines the \
             entity's Rust struct(s), then a test task. Each description must be \
             precise enough for a single agent to execute in one governed write \
             against a Rust library crate. Output the JSON object and nothing else.",
            brief = form.brief(),
        )
    }

    /// Parse the model's inner `result` text into an [`Intake`]. The model is
    /// asked for a bare JSON object; we tolerate it being wrapped in prose or a
    /// ```json fence by extracting the first balanced `{...}` span.
    ///
    /// Public + pure so the parsing contract is unit-tested directly (no process).
    pub fn parse_plan(result_text: &str) -> Result<Intake, LeadEngineerError> {
        let json = extract_json_object(result_text)
            .ok_or_else(|| LeadEngineerError::ParsePlan(format!(
                "no JSON object found in model output: {}",
                truncate(result_text, 200)
            )))?;
        let plan: Plan = serde_json::from_str(json)
            .map_err(|e| LeadEngineerError::ParsePlan(format!("{e}; raw: {}", truncate(json, 200))))?;
        if !plan.is_buildable() {
            return Err(LeadEngineerError::ParsePlan(
                "model returned a plan with zero tasks".to_string(),
            ));
        }
        Ok(Intake::Ready(plan))
    }
}

#[async_trait]
impl LeadEngineer for ClaudeLeadEngineer {
    async fn evaluate(&self, form: &IntakeForm) -> Result<Intake, LeadEngineerError> {
        let prompt = Self::build_prompt(form);

        // A read-only, JSON-output, ungoverned planning call. No MCP config and
        // no write tools: the lead engineer reasons and plans, it does not build.
        let out = tokio::process::Command::new("claude")
            .arg("-p")
            .arg(&prompt)
            .arg("--model")
            .arg(&self.model)
            .arg("--allowedTools")
            .arg("") // no tools: pure reasoning over the brief we inlined
            .arg("--dangerously-skip-permissions")
            .arg("--output-format")
            .arg("json")
            .output()
            .await
            .map_err(LeadEngineerError::Spawn)?;

        if !out.status.success() {
            return Err(LeadEngineerError::NonZeroExit {
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }

        let envelope: serde_json::Value =
            serde_json::from_slice(&out.stdout).map_err(LeadEngineerError::ParseEnvelope)?;
        let result_text = envelope["result"].as_str().unwrap_or_default();
        Self::parse_plan(result_text)
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────────

/// Extract the first balanced top-level `{...}` JSON object span from `s`,
/// tolerating surrounding prose or a ```json fence. Returns the substring (not a
/// parsed value) so the caller can deserialize into a typed [`Plan`].
fn extract_json_object(s: &str) -> Option<&str> {
    let start = s.find('{')?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in s[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[start..=start + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Truncate `s` to at most `n` chars for bounded error messages.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(n).collect();
        format!("{truncated}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::form::IntakeForm;

    #[tokio::test]
    async fn stub_produces_a_buildable_plan_for_the_sample_form() {
        let form = IntakeForm::sample_app();
        let intake = StubLeadEngineer::new().evaluate(&form).await.unwrap();
        assert!(intake.is_ready());
        let plan = intake.plan().unwrap();
        assert!(plan.is_buildable());
        assert_eq!(plan.app_name, "expense-tracker");
        // Backend task then test task.
        assert_eq!(plan.tasks.len(), 2);
        assert!(plan.tasks[0].description.contains("Expense"));
        assert!(plan.summary.contains("Expense"));
    }

    #[test]
    fn prompt_inlines_the_brief_and_demands_json() {
        let form = IntakeForm::sample_app();
        let prompt = ClaudeLeadEngineer::build_prompt(&form);
        assert!(prompt.contains("expense-tracker"));
        assert!(prompt.contains("Expense"));
        assert!(prompt.contains("\"tasks\""));
        assert!(prompt.contains("LEAD ENGINEER"));
    }

    #[test]
    fn parse_plan_accepts_a_bare_json_object() {
        let raw = r#"{"app_name":"budget-tracker","summary":"s","tasks":[
            {"role":"Implementer","kind":"backend","description":"build Expense"}]}"#;
        let intake = ClaudeLeadEngineer::parse_plan(raw).unwrap();
        let plan = intake.plan().unwrap();
        assert_eq!(plan.app_name, "budget-tracker");
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].kind, TaskKind::Backend);
    }

    #[test]
    fn parse_plan_tolerates_prose_and_a_fence_around_the_json() {
        let raw = "Sure, here is the plan:\n```json\n\
            {\"app_name\":\"x\",\"summary\":\"s\",\"tasks\":[\
            {\"role\":\"R\",\"kind\":\"test\",\"description\":\"d\"}]}\n```\nDone.";
        let intake = ClaudeLeadEngineer::parse_plan(raw).unwrap();
        assert_eq!(intake.plan().unwrap().tasks[0].kind, TaskKind::Test);
    }

    #[test]
    fn parse_plan_rejects_a_zero_task_plan() {
        let raw = r#"{"app_name":"x","summary":"s","tasks":[]}"#;
        let err = ClaudeLeadEngineer::parse_plan(raw).unwrap_err();
        assert!(matches!(err, LeadEngineerError::ParsePlan(_)));
    }

    #[test]
    fn parse_plan_rejects_non_json_output() {
        let err = ClaudeLeadEngineer::parse_plan("I cannot help with that.").unwrap_err();
        assert!(matches!(err, LeadEngineerError::ParsePlan(_)));
    }

    #[test]
    fn extract_json_object_handles_nested_braces_and_strings() {
        let s = r#"prefix {"a":{"b":1},"c":"}not the end"} suffix"#;
        let extracted = extract_json_object(s).unwrap();
        assert_eq!(extracted, r#"{"a":{"b":1},"c":"}not the end"}"#);
    }
}
