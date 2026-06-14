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
//!
//! ## Staff-engineer behavior (CONSUMER_UX.md §"The lead engineer's behavior")
//!
//! - **Checklist-driven**: [`LeadEngineerResponse`] carries an explicit
//!   [`ChecklistItem`] list so the UI can show progress. Each item is either
//!   `resolved` (the PO answered it) or `open` (still needs a reply).
//! - **Confidence-scored**: a [`ConfidenceScore`] (0–100) rises as checklist
//!   items are resolved. It is the honest signal of "how ready am I to build."
//! - **Proactively suggestive**: [`ProductSuggestion`] carries things the PO did
//!   not think of (admin/RBAC alongside login, soft-delete, audit log, etc.),
//!   explained in plain language.
//! - **Honest about limits**: [`HonestyVerdict`] distinguishes `Proceed` from
//!   `RecommendArchitect` (needs a human in the loop) and `TooComplex` (beyond
//!   Camerata's reach). The clarify driver surfaces these; it never silently falls
//!   back.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::form::IntakeForm;
use crate::plan::{Plan, PlanTask, TaskKind};
use crate::story::StoryId;
use crate::Intake;

// ─── confidence score newtype ─────────────────────────────────────────────────

/// A 0–100 confidence score: how ready the lead engineer is to build well.
///
/// 0 = no idea yet; 100 = fully pinned down. It rises as [`ChecklistItem`]s are
/// resolved. The UI renders it as a progress signal so the PO understands the
/// trade-off of skipping ahead ("lower-confidence build is your call").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ConfidenceScore(u8);

impl ConfidenceScore {
    /// Construct a score, clamping to [0, 100].
    pub fn new(raw: u8) -> Self {
        Self(raw.min(100))
    }

    /// The raw value in [0, 100].
    pub fn value(self) -> u8 {
        self.0
    }

    /// Whether the score is high enough to build confidently (>= 80 by
    /// convention; the lead engineer can still ask clarifying questions below
    /// this threshold).
    pub fn is_build_ready(self) -> bool {
        self.0 >= 80
    }
}

// ─── checklist item ───────────────────────────────────────────────────────────

/// One item on the lead engineer's clarification checklist.
///
/// Each item represents something the engineer needs pinned down before it can
/// build well. The UI shows the full list with resolved/open status so the PO
/// can see how close the engineer is to "ready."
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChecklistItem {
    /// Short identifier, stable across turns (e.g. `"auth_model"`,
    /// `"currency"`, `"soft_delete"`). Snake_case; used for dedup and progress
    /// tracking.
    pub id: String,
    /// The plain-language question or concern, written for a non-technical PO.
    pub question: String,
    /// A one-sentence explanation of WHY this matters, so the PO understands
    /// it rather than feeling interrogated.
    pub reason: String,
    /// `true` once the PO has answered this item (resolved by a prior Q&A
    /// round). New items start `false`.
    #[serde(default)]
    pub resolved: bool,
}

impl ChecklistItem {
    /// Construct an open (unresolved) checklist item.
    pub fn open(
        id: impl Into<String>,
        question: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            question: question.into(),
            reason: reason.into(),
            resolved: false,
        }
    }

    /// Construct a resolved checklist item (used in tests / stub completions).
    pub fn resolved(
        id: impl Into<String>,
        question: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            question: question.into(),
            reason: reason.into(),
            resolved: true,
        }
    }
}

// ─── product suggestion ───────────────────────────────────────────────────────

/// A proactive product-level suggestion the lead engineer raises unprompted.
///
/// These are things a Product Owner would miss (admin panel / RBAC alongside
/// login, soft-delete instead of hard-delete, audit log, etc.), written so a
/// non-technical person understands them. The PO can accept or decline each one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProductSuggestion {
    /// Short identifier (e.g. `"admin_users"`, `"soft_delete"`, `"audit_log"`).
    pub id: String,
    /// The suggestion in plain language, written for a non-technical PO.
    /// Example: "You added login. Apps like this usually also need a place for
    /// an admin to manage users and decide who can do what."
    pub suggestion: String,
    /// The rationale in one sentence, so the PO can decide without needing
    /// engineering context.
    pub rationale: String,
    /// The [`StoryId`](crate::story::StoryId) this suggestion is about, when it
    /// arises from a specific user story (e.g. an "admin area" suggestion that
    /// references the "log in" story). `None` for project-wide suggestions that
    /// do not attach to one story. The UI renders the link so the PO sees WHICH
    /// part of their app the suggestion concerns.
    #[serde(default)]
    pub story_id: Option<StoryId>,
}

impl ProductSuggestion {
    /// Construct a project-wide product suggestion (not tied to one story).
    pub fn new(
        id: impl Into<String>,
        suggestion: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            suggestion: suggestion.into(),
            rationale: rationale.into(),
            story_id: None,
        }
    }

    /// Construct a product suggestion that references a specific user story.
    pub fn for_story(
        id: impl Into<String>,
        story_id: StoryId,
        suggestion: impl Into<String>,
        rationale: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            suggestion: suggestion.into(),
            rationale: rationale.into(),
            story_id: Some(story_id),
        }
    }

    /// Attach (or replace) the referenced story id. Builder form.
    pub fn referencing(mut self, story_id: StoryId) -> Self {
        self.story_id = Some(story_id);
        self
    }
}

// ─── honesty verdict ──────────────────────────────────────────────────────────

/// The lead engineer's honest assessment of whether it can build this well.
///
/// This is the "honest about limits" dimension from CONSUMER_UX.md. It is NOT a
/// failure mode — it is a TRUST FEATURE. The UI surfaces each variant plainly:
/// `Proceed` = full speed ahead; `RecommendArchitect` = route to a human;
/// `TooComplex` = plain decline rather than building something fragile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum HonestyVerdict {
    /// The request is well-within Camerata's reach. Build when the checklist
    /// clears (or earlier, at the PO's discretion).
    Proceed,
    /// The app has complexity that warrants a human architect in the loop. The
    /// `reason` field explains why in plain language.
    RecommendArchitect {
        /// Plain-language explanation for the PO.
        reason: String,
    },
    /// The app is beyond what Camerata can build well on its own. The `reason`
    /// field says so plainly, rather than silently building something fragile.
    TooComplex {
        /// Plain-language explanation for the PO.
        reason: String,
    },
}

impl HonestyVerdict {
    /// Whether this verdict permits Camerata to attempt a build (possibly with
    /// lower confidence). `RecommendArchitect` and `TooComplex` do not.
    pub fn can_build(&self) -> bool {
        matches!(self, HonestyVerdict::Proceed)
    }
}

// ─── lead engineer response ───────────────────────────────────────────────────

/// The full, rich response from a lead engineer evaluation — everything the UI
/// needs to render the clarify step as described in CONSUMER_UX.md §2.
///
/// This is what a single `evaluate` call returns INSIDE the outer [`Intake`]
/// enum. The [`Intake`] variants then wrap or unwrap it:
///
/// - [`Intake::Ready`] — `LeadEngineerResponse` with a `Plan` AND a completed
///   checklist and full confidence.
/// - [`Intake::NeedsClarification`] — `LeadEngineerResponse` with open items,
///   the questions to ask next, and the current confidence.
/// - [`Intake::RecommendArchitect`] / [`Intake::TooComplex`] — honest limits.
///
/// Fields:
/// - `checklist`: the running list of things the engineer needs pinned down.
/// - `confidence`: the current confidence score (rises as checklist fills).
/// - `suggestions`: proactive product-level suggestions.
/// - `verdict`: the honesty assessment.
/// - `questions`: the subset of open checklist items turned into PO-facing
///   questions for this turn (empty when `Ready`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeadEngineerResponse {
    /// The running checklist of things the engineer needs to know.
    pub checklist: Vec<ChecklistItem>,
    /// Current confidence (0–100). Rises as the checklist fills.
    pub confidence: ConfidenceScore,
    /// Product-level suggestions the PO did not think of.
    #[serde(default)]
    pub suggestions: Vec<ProductSuggestion>,
    /// Honesty assessment: whether Camerata can build this well.
    #[serde(default)]
    pub verdict: HonestyVerdict,
    /// The questions to show the PO on this turn (subset of open checklist
    /// items). Empty when the engineer is ready to build.
    #[serde(default)]
    pub questions: Vec<String>,
}

impl Default for HonestyVerdict {
    fn default() -> Self {
        HonestyVerdict::Proceed
    }
}

impl LeadEngineerResponse {
    /// How many checklist items are resolved.
    pub fn resolved_count(&self) -> usize {
        self.checklist.iter().filter(|i| i.resolved).count()
    }

    /// How many checklist items are still open.
    pub fn open_count(&self) -> usize {
        self.checklist.iter().filter(|i| !i.resolved).count()
    }

    /// Whether all checklist items are resolved.
    pub fn checklist_complete(&self) -> bool {
        self.checklist.iter().all(|i| i.resolved)
    }
}

// ─── errors ───────────────────────────────────────────────────────────────────

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
///
/// The stub also produces a deterministic [`LeadEngineerResponse`] (checklist,
/// confidence, suggestions) so tests for the new staff-engineer behavior do not
/// require a network call.
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

    /// Build the deterministic [`LeadEngineerResponse`] the stub attaches to
    /// every [`Intake::Ready`] result. The checklist is fully resolved (the stub
    /// never asks questions), confidence is 90, and suggestions are derived from
    /// the form shape (e.g. a login suggestion when no auth role is declared).
    ///
    /// Public so tests can assert against the exact stub response shape without
    /// going through `evaluate`.
    pub fn response_for(form: &IntakeForm) -> LeadEngineerResponse {
        let entity_names: Vec<&str> = form.entities.iter().map(|e| e.name.as_str()).collect();

        // Deterministic checklist — two items, both pre-resolved (stub is already
        // "sure").
        let checklist = vec![
            ChecklistItem::resolved(
                "entity_scope",
                format!(
                    "Which entities does the app track? (Found: {})",
                    entity_names.join(", ")
                ),
                "The entity list is the spine of the data model.",
            ),
            ChecklistItem::resolved(
                "user_roles",
                "Who are the kinds of people that use the app?",
                "User roles drive access control and the navigation structure.",
            ),
        ];

        // Proactive suggestions derived from form shape.
        let mut suggestions: Vec<ProductSuggestion> = Vec::new();

        let has_auth_role = form.roles.iter().any(|r| {
            let name = r.name.to_lowercase();
            name.contains("admin") || name.contains("owner") || name.contains("member")
        });
        if has_auth_role {
            suggestions.push(ProductSuggestion::new(
                "admin_users",
                "You have roles that imply login. Apps like this usually also need a place \
                 for an admin to manage users and decide who can do what. Want me to include \
                 a simple users-and-permissions area?",
                "Without an admin panel, adding or removing users requires direct database \
                 access, which is not workable for a real app.",
            ));
        }

        let has_removable = form
            .entities
            .iter()
            .any(|e| e.capabilities.can_remove);
        if has_removable {
            suggestions.push(ProductSuggestion::new(
                "soft_delete",
                "You have entities that can be removed. Should removed items disappear \
                 permanently, or should they be hidden but recoverable (soft delete)?",
                "Permanent deletion is irreversible. Soft-delete lets you or the PO restore \
                 an item that was removed by accident.",
            ));
        }

        LeadEngineerResponse {
            checklist,
            confidence: ConfidenceScore::new(90),
            suggestions,
            verdict: HonestyVerdict::Proceed,
            questions: vec![], // stub is always ready; no questions.
        }
    }
}

#[async_trait]
impl LeadEngineer for StubLeadEngineer {
    async fn evaluate(&self, form: &IntakeForm) -> Result<Intake, LeadEngineerError> {
        Ok(Intake::Ready {
            plan: Self::plan_for(form),
            response: Self::response_for(form),
        })
    }
}

// ─── the real Claude lead engineer ───────────────────────────────────────────

/// The default model id the live lead-engineer call uses. Behind the seam, so
/// only this concrete type names a model.
pub const DEFAULT_LEAD_ENGINEER_MODEL: &str = "claude-sonnet-4-5";

/// The REAL lead engineer: a headless `claude -p` call that evaluates the form
/// as a staff engineer and returns a strict-JSON [`ModelOutput`] (checklist,
/// confidence, suggestions, honesty verdict, and optionally a plan).
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

/// The shape the model must return from a single `claude -p` call.
///
/// This is the internal deserialization target; the public API surfaces
/// [`Intake`] + [`LeadEngineerResponse`].
#[derive(Debug, Clone, Deserialize)]
struct ModelOutput {
    /// The app name (echoed or normalized from the form).
    app_name: String,
    /// A one-paragraph engineering summary.
    summary: String,
    /// The ordered build tasks. Empty when `needs_clarification` is true or when
    /// the verdict is `recommend_architect` / `too_complex`.
    #[serde(default)]
    tasks: Vec<crate::plan::PlanTask>,
    /// Whether the model wants clarification before committing to a plan.
    #[serde(default)]
    needs_clarification: bool,
    /// The clarify-turn questions for the PO (present when
    /// `needs_clarification` is true).
    #[serde(default)]
    questions: Vec<String>,
    /// The running checklist of things the model needs to know.
    #[serde(default)]
    checklist: Vec<ChecklistItem>,
    /// Confidence score 0–100.
    #[serde(default)]
    confidence: u8,
    /// Proactive product-level suggestions.
    #[serde(default)]
    suggestions: Vec<ProductSuggestion>,
    /// The honesty verdict.
    #[serde(default)]
    verdict: HonestyVerdict,
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

    /// Build the prompt that asks the model to act as a staff engineer: maintain
    /// a checklist, score confidence, raise product-level suggestions, and
    /// honestly flag architect-needed / too-complex situations.
    ///
    /// Pure + public so it is unit-testable without spawning a process, and so
    /// the demo can show exactly what the lead engineer was asked.
    pub fn build_prompt(form: &IntakeForm) -> String {
        format!(
            "You are a STAFF LEAD ENGINEER evaluating a Product Owner's intake \
             form for a small bespoke CRUD app. Act as an experienced Staff \
             Engineer would in a planning session: maintain a checklist of what \
             you still need to know, score your own confidence, proactively raise \
             product-level additions the PO did not think of, and be honest when \
             the request is beyond what a governed agent team can build well.\n\n\
             === INTAKE FORM ===\n{brief}\n\
             === END FORM ===\n\n\
             === YOUR JOB ===\n\
             1. CHECKLIST: Identify every gap, contradiction, and unstated \
                assumption. For each, produce a checklist item with an `id` \
                (snake_case, stable across turns), a plain-language `question` \
                for the PO, and a one-sentence `reason` explaining why it matters. \
                Mark items `resolved: true` if the form or prior Q&A already \
                answered them.\n\
             2. CONFIDENCE: Score your confidence 0–100 based on how many \
                checklist items are resolved. 0 = no idea; 100 = fully pinned \
                down. A score >= 80 means you are ready to build.\n\
             3. SUGGESTIONS: Raise product-level things the PO almost certainly \
                needs but did not ask for. Example: if they added login, suggest an \
                admin users-and-permissions area. Explain each suggestion in plain \
                language the PO understands without technical context.\n\
             4. VERDICT: Decide honestly — \"proceed\" (within Camerata's reach), \
                \"recommend_architect\" (real architectural complexity needing a \
                human in the loop), or \"too_complex\" (beyond Camerata's reach \
                entirely). Include a plain-language `reason` for non-proceed \
                verdicts.\n\
             5. PLAN: If confidence >= 80 AND verdict is \"proceed\", output a \
                build plan with ordered tasks. Otherwise leave `tasks` empty and \
                set `needs_clarification: true` with the open questions.\n\n\
             === OUTPUT FORMAT ===\n\
             Output ONLY a single JSON object (no prose, no markdown fences):\n\
             {{\n\
             \x20 \"app_name\": string,\n\
             \x20 \"summary\": string,\n\
             \x20 \"needs_clarification\": boolean,\n\
             \x20 \"questions\": [string],\n\
             \x20 \"checklist\": [\n\
             \x20   {{ \"id\": string, \"question\": string, \"reason\": string, \
                       \"resolved\": boolean }}\n\
             \x20 ],\n\
             \x20 \"confidence\": number,\n\
             \x20 \"suggestions\": [\n\
             \x20   {{ \"id\": string, \"suggestion\": string, \"rationale\": string }}\n\
             \x20 ],\n\
             \x20 \"verdict\": {{ \"verdict\": \"proceed\" }}\n\
             \x20   | {{ \"verdict\": \"recommend_architect\", \"reason\": string }}\n\
             \x20   | {{ \"verdict\": \"too_complex\", \"reason\": string }},\n\
             \x20 \"tasks\": [\n\
             \x20   {{ \"role\": string, \"kind\": \"database\"|\"backend\"|\"frontend\"|\"test\",\n\
             \x20      \"description\": string }}\n\
             \x20 ]\n\
             }}\n\n\
             Keep plans small and CRUD-shaped. Each task description must be \
             precise enough for a single governed agent. Output the JSON object \
             and nothing else.",
            brief = form.brief(),
        )
    }

    /// Parse the model's inner `result` text into an [`Intake`]. The model is
    /// asked for a bare JSON object; we tolerate it being wrapped in prose or a
    /// ```json fence by extracting the first balanced `{...}` span.
    ///
    /// Public + pure so the parsing contract is unit-tested directly (no process).
    pub fn parse_response(result_text: &str) -> Result<Intake, LeadEngineerError> {
        let json = extract_json_object(result_text)
            .ok_or_else(|| LeadEngineerError::ParsePlan(format!(
                "no JSON object found in model output: {}",
                truncate(result_text, 200)
            )))?;
        let output: ModelOutput = serde_json::from_str(json)
            .map_err(|e| LeadEngineerError::ParsePlan(format!("{e}; raw: {}", truncate(json, 200))))?;

        let response = LeadEngineerResponse {
            checklist: output.checklist,
            confidence: ConfidenceScore::new(output.confidence),
            suggestions: output.suggestions,
            verdict: output.verdict.clone(),
            questions: output.questions.clone(),
        };

        // Honesty short-circuits: surface non-proceed verdicts immediately.
        match &output.verdict {
            HonestyVerdict::RecommendArchitect { reason } => {
                return Ok(Intake::RecommendArchitect {
                    reason: reason.clone(),
                    response,
                });
            }
            HonestyVerdict::TooComplex { reason } => {
                return Ok(Intake::TooComplex {
                    reason: reason.clone(),
                    response,
                });
            }
            HonestyVerdict::Proceed => {}
        }

        if output.needs_clarification || output.tasks.is_empty() {
            if output.questions.is_empty() {
                return Err(LeadEngineerError::ParsePlan(
                    "model set needs_clarification but supplied no questions".to_string(),
                ));
            }
            return Ok(Intake::NeedsClarification {
                questions: output.questions,
                response,
            });
        }

        let plan = Plan {
            app_name: output.app_name,
            summary: output.summary,
            tasks: output.tasks,
        };
        if !plan.is_buildable() {
            return Err(LeadEngineerError::ParsePlan(
                "model returned a plan with zero tasks".to_string(),
            ));
        }
        Ok(Intake::Ready { plan, response })
    }

    /// Back-compat alias: parse a bare plan from text (used in existing tests
    /// that pre-date the staff-engineer shape). Wraps the result in a minimal
    /// `LeadEngineerResponse` with no checklist, 100 confidence, no suggestions.
    pub fn parse_plan(result_text: &str) -> Result<Intake, LeadEngineerError> {
        // Try the new rich format first.
        if let Ok(intake) = Self::parse_response(result_text) {
            return Ok(intake);
        }
        // Fall back to bare Plan JSON (the old format, for backward-compat tests).
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
        Ok(Intake::Ready {
            plan,
            response: LeadEngineerResponse {
                checklist: vec![],
                confidence: ConfidenceScore::new(100),
                suggestions: vec![],
                verdict: HonestyVerdict::Proceed,
                questions: vec![],
            },
        })
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
        Self::parse_response(result_text)
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

    #[tokio::test]
    async fn stub_response_has_checklist_confidence_and_suggestions() {
        let form = IntakeForm::sample_app();
        let intake = StubLeadEngineer::new().evaluate(&form).await.unwrap();
        let response = intake.response();
        // Stub always produces confidence 90.
        assert_eq!(response.confidence.value(), 90);
        // All checklist items pre-resolved.
        assert!(!response.checklist.is_empty());
        assert!(response.checklist.iter().all(|i| i.resolved));
        // Proceed verdict.
        assert!(matches!(response.verdict, HonestyVerdict::Proceed));
        // Suggestions include admin_users (Owner role in sample_app).
        assert!(
            response.suggestions.iter().any(|s| s.id == "admin_users"),
            "expected admin_users suggestion for Owner role"
        );
    }

    #[tokio::test]
    async fn stub_can_build_check() {
        let form = IntakeForm::sample_app();
        let intake = StubLeadEngineer::new().evaluate(&form).await.unwrap();
        assert!(intake.can_build());
    }

    #[test]
    fn prompt_inlines_the_brief_and_demands_json() {
        let form = IntakeForm::sample_app();
        let prompt = ClaudeLeadEngineer::build_prompt(&form);
        assert!(prompt.contains("expense-tracker"));
        assert!(prompt.contains("Expense"));
        assert!(prompt.contains("\"tasks\""));
        assert!(prompt.contains("STAFF LEAD ENGINEER"));
        // Staff-engineer additions are in the prompt.
        assert!(prompt.contains("checklist"));
        assert!(prompt.contains("confidence"));
        assert!(prompt.contains("suggestions"));
        assert!(prompt.contains("verdict"));
    }

    #[test]
    fn parse_response_accepts_rich_json_ready() {
        let raw = r#"{
            "app_name": "budget-tracker",
            "summary": "expense CRUD",
            "needs_clarification": false,
            "questions": [],
            "checklist": [
                {"id": "entity_scope", "question": "What entities?", "reason": "needed", "resolved": true}
            ],
            "confidence": 85,
            "suggestions": [
                {"id": "soft_delete", "suggestion": "Consider soft delete", "rationale": "reversible"}
            ],
            "verdict": {"verdict": "proceed"},
            "tasks": [
                {"role": "Implementer", "kind": "backend", "description": "define Expense"}
            ]
        }"#;
        let intake = ClaudeLeadEngineer::parse_response(raw).unwrap();
        assert!(intake.is_ready());
        let plan = intake.plan().unwrap();
        assert_eq!(plan.app_name, "budget-tracker");
        let response = intake.response();
        assert_eq!(response.confidence.value(), 85);
        assert_eq!(response.checklist.len(), 1);
        assert!(response.checklist[0].resolved);
        assert_eq!(response.suggestions.len(), 1);
        assert_eq!(response.suggestions[0].id, "soft_delete");
        assert!(matches!(response.verdict, HonestyVerdict::Proceed));
    }

    #[test]
    fn parse_response_accepts_needs_clarification() {
        let raw = r#"{
            "app_name": "my-app",
            "summary": "unclear",
            "needs_clarification": true,
            "questions": ["Which currency?", "What roles?"],
            "checklist": [
                {"id": "currency", "question": "Which currency?", "reason": "for money fields", "resolved": false}
            ],
            "confidence": 40,
            "suggestions": [],
            "verdict": {"verdict": "proceed"},
            "tasks": []
        }"#;
        let intake = ClaudeLeadEngineer::parse_response(raw).unwrap();
        assert!(!intake.is_ready());
        assert_eq!(intake.questions(), &["Which currency?".to_string(), "What roles?".to_string()]);
        let response = intake.response();
        assert_eq!(response.confidence.value(), 40);
        assert_eq!(response.open_count(), 1);
    }

    #[test]
    fn parse_response_surfaces_recommend_architect() {
        let raw = r#"{
            "app_name": "complex-app",
            "summary": "too hard",
            "needs_clarification": false,
            "questions": [],
            "checklist": [],
            "confidence": 20,
            "suggestions": [],
            "verdict": {"verdict": "recommend_architect", "reason": "needs event sourcing"},
            "tasks": []
        }"#;
        let intake = ClaudeLeadEngineer::parse_response(raw).unwrap();
        assert!(!intake.is_ready());
        assert!(!intake.can_build());
        assert!(matches!(intake, crate::Intake::RecommendArchitect { .. }));
    }

    #[test]
    fn parse_response_surfaces_too_complex() {
        let raw = r#"{
            "app_name": "ml-app",
            "summary": "way too hard",
            "needs_clarification": false,
            "questions": [],
            "checklist": [],
            "confidence": 5,
            "suggestions": [],
            "verdict": {"verdict": "too_complex", "reason": "ML pipeline required"},
            "tasks": []
        }"#;
        let intake = ClaudeLeadEngineer::parse_response(raw).unwrap();
        assert!(!intake.is_ready());
        assert!(!intake.can_build());
        assert!(matches!(intake, crate::Intake::TooComplex { .. }));
    }

    // Back-compat: parse_plan still accepts the old bare plan format.
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
        // Old bare format with empty tasks — should fail even via back-compat path.
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

    #[test]
    fn honesty_verdict_can_build_only_for_proceed() {
        assert!(HonestyVerdict::Proceed.can_build());
        assert!(!HonestyVerdict::RecommendArchitect { reason: "x".into() }.can_build());
        assert!(!HonestyVerdict::TooComplex { reason: "x".into() }.can_build());
    }

    #[test]
    fn confidence_score_clamping_and_build_ready() {
        assert_eq!(ConfidenceScore::new(200).value(), 100);
        assert!(ConfidenceScore::new(80).is_build_ready());
        assert!(!ConfidenceScore::new(79).is_build_ready());
    }

    #[test]
    fn checklist_item_round_trips_json() {
        let item = ChecklistItem::open("currency", "Which currency?", "needed for money fields");
        let json = serde_json::to_string(&item).unwrap();
        let back: ChecklistItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "currency");
        assert!(!back.resolved);
    }

    #[test]
    fn product_suggestion_round_trips_json() {
        let sug = ProductSuggestion::new("admin_users", "You need an admin panel", "without it you need DB access");
        let json = serde_json::to_string(&sug).unwrap();
        let back: ProductSuggestion = serde_json::from_str(&json).unwrap();
        assert_eq!(back.id, "admin_users");
        assert_eq!(back.suggestion, "You need an admin panel");
    }

    #[test]
    fn lead_engineer_response_counts() {
        let response = LeadEngineerResponse {
            checklist: vec![
                ChecklistItem::resolved("a", "q1", "r1"),
                ChecklistItem::open("b", "q2", "r2"),
            ],
            confidence: ConfidenceScore::new(60),
            suggestions: vec![],
            verdict: HonestyVerdict::Proceed,
            questions: vec![],
        };
        assert_eq!(response.resolved_count(), 1);
        assert_eq!(response.open_count(), 1);
        assert!(!response.checklist_complete());
    }
}
