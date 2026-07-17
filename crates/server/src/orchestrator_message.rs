//! `POST /api/orchestrator/message` — the first functional increment of the
//! architect-orchestrator LOOP (`docs/plans/2026-07-09_product-owner-head-vibe-mode.md`,
//! "Architect-orchestrator design (decided 2026-07-10)").
//!
//! Scope: handle a CHANGE REQUEST against an EXISTING scaffolded project, end to
//! end, real. This module wires `camerata_orchestrator_core::turn::handle_turn`'s
//! three seam traits (`TurnLlm`, `TurnExecutor`, `DecisionRecorder`) to PRODUCTION
//! backends:
//!
//! - [`RealTurnLlm`]: the REAL LLM (`AppState::llm()`), never a canned response. A
//!   model/transport failure is an honest `Err` (see `TurnLlm::interpret`'s doc).
//! - [`RealTurnExecutor`]: the REAL governed execution seam. It reuses
//!   `crate::start_governed_run` (private to the crate root, but visible here as a
//!   descendant module) EXACTLY as `POST /api/stories/:id/run` does — it does not
//!   fork or reimplement any of that machinery. Because that seam operates on an
//!   existing `CanonicalStory` + an approved `DecisionRecord` on its UoW, this
//!   executor materializes both from the orchestrator's own proposed change (the
//!   orchestrator IS the "architect" approving its own Class A/B pick here, per the
//!   plan doc: "one of the safest things to automate").
//! - A `DecisionRecorder` closure wrapping `AppState::record_orchestrator_decision`.
//!
//! # Honest error, never a placeholder
//! If the project has no repo configured, or the repo does not resolve to a local
//! checkout, or no live LLM is available, the handler returns
//! `{ "ok": false, "error": "..." }` — never a fabricated `TurnOutcome`.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;

use camerata_orchestrator_core::turn::{
    DecisionRecorder, ExecutionSummary, Interpretation, ProposedChange, RecordedDecisionInput,
    TouchedArea, TurnDeps, TurnExecutor, TurnLlm, TurnOutcome,
};

use crate::llm::{Llm, LlmRequest};
use crate::AppState;

// ─── request / response wire shapes ─────────────────────────────────────────────

#[derive(Debug, serde::Deserialize)]
pub struct OrchestratorMessageReq {
    pub project_id: String,
    pub message: String,
}

/// Build the JSON body for one [`TurnOutcome`]. Kept as a free function (not a
/// `Serialize` impl on the orchestrator-core type) because that crate deliberately
/// carries no serde dependency — see `crates/orchestrator/src/turn.rs`'s doc.
fn outcome_to_json(outcome: &TurnOutcome) -> serde_json::Value {
    match outcome {
        TurnOutcome::Applied { summary, decision } => serde_json::json!({
            "ok": true,
            "kind": "applied",
            "summary": summary,
            "decision": decision_to_json(decision),
        }),
        TurnOutcome::NeedsApproval { consequence, decision } => serde_json::json!({
            "ok": true,
            "kind": "needs_approval",
            "consequence": consequence,
            "decision": decision_to_json(decision),
        }),
        TurnOutcome::NeedsClarification { questions } => serde_json::json!({
            "ok": true,
            "kind": "needs_clarification",
            "questions": questions,
        }),
    }
}

fn decision_to_json(decision: &camerata_orchestrator_core::turn::DecisionSummary) -> serde_json::Value {
    serde_json::json!({
        "class": decision.class.as_str(),
        "confidence": decision.confidence.as_str(),
        "id": decision.id,
    })
}

/// `POST /api/orchestrator/message` handler. Builds the REAL seams from `state`
/// and drives one [`camerata_orchestrator_core::turn::handle_turn`] round trip.
pub async fn orchestrator_message(
    State(state): State<AppState>,
    Json(req): Json<OrchestratorMessageReq>,
) -> Response {
    let Some(project) = state.projects().get(&req.project_id) else {
        return honest_error(
            axum::http::StatusCode::NOT_FOUND,
            format!("no project with id `{}`", req.project_id),
        );
    };
    let Some(repo) = project.repos.first().cloned() else {
        return honest_error(
            axum::http::StatusCode::CONFLICT,
            format!("project `{}` has no repo configured", req.project_id),
        );
    };
    let Some(repo_dir) = crate::workspace::resolve_repo_dir(
        state.settings().repo_path(&repo).as_deref(),
        state.settings().workspace_root().as_deref(),
        &repo,
    ) else {
        return honest_error(
            axum::http::StatusCode::CONFLICT,
            format!(
                "repo `{repo}` is not resolved to a local checkout — set its path in the \
                 Rules view before sending it a change request"
            ),
        );
    };
    if !repo_dir.is_dir() {
        return honest_error(
            axum::http::StatusCode::CONFLICT,
            format!(
                "repo `{repo}`'s configured local path `{}` does not exist on disk",
                repo_dir.display()
            ),
        );
    }

    let llm = RealTurnLlm { llm: state.llm() };
    let executor = RealTurnExecutor {
        state: state.clone(),
        repo,
    };
    let recorder = RealDecisionRecorder { state: state.clone() };
    let deps = TurnDeps {
        llm: &llm,
        executor: &executor,
        recorder: &recorder,
    };

    let turn_id = format!("orchestrator-turn-{}", chrono::Utc::now().timestamp_millis());
    match camerata_orchestrator_core::turn::handle_turn(&deps, &repo_dir, &turn_id, &req.message)
        .await
    {
        Ok(outcome) => Json(outcome_to_json(&outcome)).into_response(),
        Err(e) => honest_error(axum::http::StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
    }
}

fn honest_error(status: axum::http::StatusCode, error: String) -> Response {
    (status, Json(serde_json::json!({ "ok": false, "error": error }))).into_response()
}

// ─── RealTurnLlm: the real LLM seam ─────────────────────────────────────────────

/// Interprets a change-request message with the REAL LLM (`AppState::llm()`). Asks
/// for strict JSON so [`Interpretation`] can be built structurally rather than by
/// re-parsing prose. A model/transport failure is an honest `Err` — this NEVER
/// synthesizes a canned `Interpretation` when the model call fails.
struct RealTurnLlm {
    llm: Llm,
}

const INTERPRET_SYSTEM_PROMPT: &str = r#"You are the interpretation step of a software change-request \
orchestrator. Given the CURRENT LIVING SPEC of an existing, already-scaffolded Rust \
application and a user's free-text change request, decide ONE of two things:

1. The request is clear enough to propose a concrete change. Respond with STRICT JSON:
{"needs_clarification": false, "title": "<short title>", "description": "<what will change and why>", "touches": [{"path": "<repo-relative file path>", "adds_dependency": null}], "assumption": null or "<one sentence assumption you are making instead of asking>"}

2. The request is genuinely ambiguous in a way that would waste real work if guessed \
wrong (not a trivial style choice). Respond with STRICT JSON:
{"needs_clarification": true, "questions": ["<question 1>", "<question 2>"]}

Rules:
- ALWAYS name at least one concrete "touches" path when proposing a change — a vague \
  path like "the frontend" is not acceptable; name the actual file(s) you expect to \
  change, grounded in the living spec's description of the app's structure when possible.
- Prefer proposing over clarifying: only ask when a wrong guess would waste real work \
  (per the product's clarification-first-only-when-load-bearing policy). Cap questions \
  at 3, phrased for a non-technical product owner.
- Output ONLY the JSON object. No markdown fences, no prose before or after."#;

#[async_trait::async_trait]
impl TurnLlm for RealTurnLlm {
    async fn interpret(
        &self,
        spec: &camerata_orchestrator_core::spec::LivingSpec,
        message: &str,
    ) -> anyhow::Result<Interpretation> {
        let prompt = format!(
            "## Living spec summary\n\n{}\n\n## User's change request\n\n{message}",
            spec.summary
        );
        let req = LlmRequest::new(prompt).with_system(INTERPRET_SYSTEM_PROMPT);
        let resp = self
            .llm
            .complete(req)
            .await
            .map_err(|e| anyhow::anyhow!("live LLM required to interpret the change request: {e}"))?;
        parse_interpretation(&resp.text)
    }
}

/// Parse the model's JSON response into an [`Interpretation`]. Tolerant of a
/// fenced `json` code block (some models wrap JSON even when told not to);
/// otherwise expects the raw text to parse directly. A response that is neither
/// valid JSON nor matches either expected shape is an honest error — never
/// silently treated as an empty proposal.
fn parse_interpretation(text: &str) -> anyhow::Result<Interpretation> {
    let candidate = extract_json_object(text);
    let v: serde_json::Value = serde_json::from_str(candidate).map_err(|e| {
        anyhow::anyhow!("could not parse the model's response as JSON: {e}\nresponse was: {text}")
    })?;

    let needs_clarification = v["needs_clarification"].as_bool().unwrap_or(false);
    if needs_clarification {
        let questions: Vec<String> = v["questions"]
            .as_array()
            .map(|arr| arr.iter().filter_map(|q| q.as_str().map(String::from)).collect())
            .unwrap_or_default();
        if questions.is_empty() {
            anyhow::bail!(
                "model set needs_clarification=true but returned no questions: {text}"
            );
        }
        return Ok(Interpretation::Clarify(questions));
    }

    let title = v["title"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("model's proposal is missing a non-empty `title`: {text}"))?
        .to_string();
    let description = v["description"].as_str().unwrap_or_default().to_string();
    let touches: Vec<TouchedArea> = v["touches"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let path = t["path"].as_str()?.to_string();
                    let adds_dependency = t["adds_dependency"].as_bool();
                    Some(TouchedArea { path, adds_dependency })
                })
                .collect()
        })
        .unwrap_or_default();
    let assumption = v["assumption"].as_str().map(String::from);

    Ok(Interpretation::Propose(ProposedChange {
        title,
        description,
        touches,
        assumption,
    }))
}

/// Strip a fenced `json` code block (or a bare fence) if present; otherwise return
/// the trimmed text unchanged. Real models occasionally wrap JSON in a fence even
/// when explicitly told not to.
fn extract_json_object(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("```json") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else if let Some(rest) = trimmed.strip_prefix("```") {
        rest.strip_suffix("```").unwrap_or(rest).trim()
    } else {
        trimmed
    }
}

// ─── RealTurnExecutor: the real governed execution seam ────────────────────────

/// Drives the REAL `start_governed_run` seam for a Class A/B proposed change,
/// against `repo`'s existing local checkout. See this module's doc for why this
/// materializes a fresh `CanonicalStory` + a pre-approved `DecisionRecord`: it is
/// the same precondition `POST /api/stories/:id/run` enforces
/// (`ensure_development_gate`), just satisfied programmatically instead of via a
/// human clicking through the investigation/decision UI first — the orchestrator's
/// OWN classify+record IS the "architect approval" for a Class A/B change (see the
/// plan doc's rationale for why rule/option selection is safe to automate).
struct RealTurnExecutor {
    state: AppState,
    repo: String,
}

/// Backstop against a governed run that never reaches a terminal state (mirrors
/// `crate::lib`'s `stamp_provenance_when_done`'s `SAFETY_TIMEOUT`) — NOT the
/// expected normal duration; a healthy run finishes well before this and the
/// caller learns the real outcome then.
const EXECUTE_SAFETY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(6 * 60 * 60);

#[async_trait::async_trait]
impl TurnExecutor for RealTurnExecutor {
    async fn execute(&self, change: &ProposedChange) -> anyhow::Result<ExecutionSummary> {
        let now = chrono::Utc::now();
        let story_id = format!("{}#{}", self.repo, now.timestamp_millis());

        // 1. Register the change as a canonical story targeting this repo.
        let story = camerata_worktracker::CanonicalStory {
            id: story_id.clone(),
            external_ref: None,
            title: change.title.clone(),
            description: change.description.clone(),
            status: camerata_worktracker::FeatureStatus::Intake,
            created_by: "orchestrator".to_string(),
            targets: vec![camerata_worktracker::RepoTarget::new(self.repo.clone())],
        };
        self.state
            .stories
            .upsert(story)
            .await
            .map_err(|e| anyhow::anyhow!("could not register the change as a story: {e}"))?;

        // 2. Approve a single decision record so `ensure_development_gate` passes —
        //    this Class A/B change IS the orchestrator's own approved decision.
        let decision = camerata_worktracker::investigation::DecisionRecord::ai_proposed(
            story_id.clone(),
            format!("{story_id}/decision/orchestrator-change"),
            change.title.clone(),
            format!("Orchestrator change request: {}", change.title),
            change.description.clone(),
            Vec::new(),
            now,
        )
        .approve(now);
        self.state.uow().set_decisions(&story_id, vec![decision]);

        // 3. Give the UoW a branch so `start_governed_run` resolves the brownfield
        //    worktree (an absent branch falls back to the greenfield scaffolder,
        //    which is NOT what a change against an EXISTING project wants).
        let branch = format!("camerata/orchestrator/{}", now.timestamp_millis());
        self.state.uow().set_branch(&story_id, Some(branch));

        // 4. The no-code-first gate (defense in depth — should always pass given
        //    steps 1-3, but never bypass it).
        crate::ensure_development_gate(&self.state, &story_id)
            .map_err(|reason| anyhow::anyhow!("development gate refused the change: {reason}"))?;

        // 5. Drive the REAL governed execution seam — the exact function
        //    `POST /api/stories/:id/run` calls. Not forked, not reimplemented.
        let (run_id, _mode) = crate::start_governed_run(
            &self.state,
            &story_id,
            None,
            None,
            false,
            crate::run::RunKind::Watched,
        )
        .await;

        // 6. Wait for the run to reach a terminal state and report the REAL outcome.
        let run = self
            .state
            .runs
            .wait_until_done(&run_id, EXECUTE_SAFETY_TIMEOUT)
            .await;
        summarize_run(&run_id, run)
    }
}

fn summarize_run(run_id: &str, run: Option<crate::run::Run>) -> anyhow::Result<ExecutionSummary> {
    let Some(run) = run else {
        anyhow::bail!(
            "governed run {run_id} did not reach a terminal state within the safety timeout — \
             poll GET /api/runs/{run_id} for its live status"
        );
    };
    match run.status {
        crate::run::RunStatus::AwaitingQa => Ok(ExecutionSummary {
            run_id: run_id.to_string(),
            summary: format!(
                "Governed run {run_id} completed and is awaiting QA sign-off ({} gate event(s)).",
                run.events.len()
            ),
        }),
        crate::run::RunStatus::Failed { reason } => {
            anyhow::bail!("governed run {run_id} failed: {reason}")
        }
        crate::run::RunStatus::Cancelled => {
            anyhow::bail!("governed run {run_id} was cancelled before completing")
        }
        other => anyhow::bail!(
            "governed run {run_id} ended in an unexpected non-terminal status {other:?}"
        ),
    }
}

// ─── RealDecisionRecorder: wraps AppState::record_orchestrator_decision ────────

struct RealDecisionRecorder {
    state: AppState,
}

#[async_trait::async_trait]
impl DecisionRecorder for RealDecisionRecorder {
    async fn record(&self, decision: RecordedDecisionInput) -> Option<i64> {
        let mut record = camerata_persistence::OrchestratorDecision::new(
            decision.run_id,
            decision.class.as_str(),
            decision.confidence.as_str(),
            decision.chosen,
        )
        .with_alternatives(decision.alternatives);
        if let Some(assumption) = decision.assumption {
            record = record.with_assumption(assumption);
        }
        self.state.record_orchestrator_decision(record).await
    }
}
