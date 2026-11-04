//! Decomposition, routine and escalation handlers.

use crate::*;

/// Propose the component children for a parent story (not yet created). The architect
/// reviews/edits these, then commits.
pub(crate) async fn decompose_propose(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Result<Json<Vec<ProposedChild>>, AppError> {
    let parent = state
        .stories
        .get(&story_id)
        .await
        .map_err(AppError)?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("story not found: {story_id}")))?;
    // AI decomposition (grounded children), with the deterministic propose as fallback.
    let llm = state.step_llm().map_err(backend_refused)?;
    // Decomposition is a NON-FLEET step: model from the active project's per-step config.
    let model = step_model(&state, crate::project::StepKind::Decomposition);
    let grounding = state.project_grounding().await;
    // MULTI-REPO READ: ALL the active project's local repo clones (read-only across all).
    let repo_dirs = state.active_repo_dirs();
    let children = crate::decompose::propose_ai(
        &parent,
        &Practice::default_feature(),
        &llm,
        &model,
        grounding.as_deref(),
        &repo_dirs,
    )
    .await;
    Ok(Json(children))
}

/// AI-suggested clarifying questions for a story: the questions an engineer genuinely
/// needs answered before building it. The architect reviews/edits before any is posted
/// to the team (the clarify-bridge stays review-then-post). Empty on model failure.
pub(crate) async fn suggest_clarifications(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Result<Json<Vec<String>>, AppError> {
    let story = state
        .stories
        .get(&story_id)
        .await
        .map_err(AppError)?
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("story not found: {story_id}")))?;
    let system = "You are the engineer about to build this story. List the clarifying \
        questions you GENUINELY need answered before writing code: ambiguities, missing \
        decisions, edge cases, unstated constraints. Be specific to this story. Return \
        ONLY a JSON array of question strings, e.g. [\"q1\", \"q2\"]. 0-6 questions.";
    let user = format!(
        "Story: {}\n\nDescription: {}",
        story.title, story.description
    );
    let llm = state.step_llm().map_err(backend_refused)?;
    // Clarification authoring is a NON-FLEET step: model from the active project's per-step
    // config (DEFAULT_MODEL floor only when there is no active project).
    let model = step_model(&state, crate::project::StepKind::Clarification);
    let questions = match llm
        .complete(
            crate::llm::LlmRequest::new(user)
                .with_model(model)
                .with_system(system),
        )
        .await
    {
        Ok(resp) => parse_string_array(&resp.text),
        Err(_) => Vec::new(),
    };
    Ok(Json(questions))
}

/// Extract a JSON array of strings from a model response (tolerant of surrounding prose).
pub(crate) fn parse_string_array(raw: &str) -> Vec<String> {
    let (Some(start), Some(end)) = (raw.find('['), raw.rfind(']')) else {
        return Vec::new();
    };
    if end <= start {
        return Vec::new();
    }
    serde_json::from_str::<Vec<String>>(&raw[start..=end]).unwrap_or_default()
}

/// The edited set of children to commit.
#[derive(serde::Deserialize)]
pub(crate) struct CommitChildrenReq {
    pub(crate) children: Vec<ProposedChild>,
}

/// Commit the (edited) children: create each as a real story on the spine, linked to
/// the parent. The tracker write-back (as the right work-item type, with parent/child
/// relationship metadata) is the provider phase.
pub(crate) async fn decompose_commit(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
    Json(req): Json<CommitChildrenReq>,
) -> Result<Json<Vec<CanonicalStory>>, AppError> {
    let mut created = Vec::new();
    let mut child_ids = Vec::new();
    for pc in &req.children {
        let child = to_story(&story_id, pc);
        state
            .stories
            .upsert(child.clone())
            .await
            .map_err(AppError)?;
        child_ids.push(child.id.clone());
        created.push(child);
    }
    state.decompositions.record(&story_id, child_ids);
    Ok(Json(created))
}

/// The committed children of a parent story.
pub(crate) async fn list_children(
    State(state): State<AppState>,
    Path(story_id): Path<String>,
) -> Result<Json<Vec<CanonicalStory>>, AppError> {
    let mut children = Vec::new();
    for cid in state.decompositions.children_of(&story_id) {
        if let Some(story) = state.stories.get(&cid).await.map_err(AppError)? {
            children.push(story);
        }
    }
    Ok(Json(children))
}

/// Routines for the active project. No active project → empty list.
/// A routine plus COMPUTED scheduling metadata for the dashboard (next fire + due-soon). Not
/// stored on the routine; recomputed from its schedule on each list. Flattened so existing clients
/// that read the routine fields keep working and just gain the extra fields.
#[derive(serde::Serialize)]
pub(crate) struct RoutineWithMeta {
    #[serde(flatten)]
    pub(crate) routine: Routine,
    /// Next fire (naive-local ISO), or None for manual/unrecognized schedules and past one-offs.
    pub(crate) next_fire: Option<String>,
    /// Human label for the next fire ("Jun 30, 09:00"), for direct display.
    pub(crate) next_fire_label: Option<String>,
    /// True when the next fire is within the next 24 hours (drives the "due soon" status metric).
    pub(crate) due_soon: bool,
}

pub(crate) async fn list_routines(State(state): State<AppState>) -> Json<Vec<RoutineWithMeta>> {
    let Some(p) = state.projects.active() else {
        return Json(vec![]);
    };
    let now = chrono::Local::now().naive_local();
    let soon = now + chrono::Duration::hours(24);
    let with_meta = state
        .routines
        .list_for_project(&p.id)
        .into_iter()
        .map(|r| {
            let nf = crate::schedule::next_fire(&r.schedule, now);
            RoutineWithMeta {
                next_fire: nf.map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string()),
                next_fire_label: nf.map(|dt| dt.format("%b %d, %H:%M").to_string()),
                due_soon: nf.map(|dt| dt <= soon).unwrap_or(false),
                routine: r,
            }
        })
        .collect();
    Json(with_meta)
}

/// Create a routine.
pub(crate) async fn create_routine(
    State(state): State<AppState>,
    Json(req): Json<CreateRoutineReq>,
) -> Json<Routine> {
    Json(state.routines.create(&req))
}

/// Draft the operational prompt from the user's intent (ADR
/// routine_authoring_intent_not_prompt). The user describes WHAT they want; the
/// lead-engineer AI authors the operational prompt for them to review/edit
/// (`authored_by: claude`). If the model is unreachable it falls back to the
/// deterministic scaffold (`authored_by: scaffold`) so the form never dead-ends.
pub(crate) async fn draft_routine_prompt(
    State(state): State<AppState>,
    Json(req): Json<crate::routine::DraftPromptReq>,
) -> Json<crate::routine::DraftPromptResp> {
    let system = "You are Camerata's lead engineer. The user describes WHAT they want a \
        scheduled, governed agent routine to do. Author the OPERATIONAL PROMPT the agent \
        will run: concrete directives, the model tier(s) appropriate to the work, the \
        permission scope, and the governance framing (every write passes the deny-before-\
        execute gate; the agent cannot run git). Return ONLY the operational prompt text, \
        ready to run — no preamble, no markdown headers.";
    let user = format!(
        "Permission scope: {}\n\nWhat the user wants:\n{}",
        req.scope, req.intent
    );
    let scaffold = || {
        Json(crate::routine::DraftPromptResp {
            prompt: crate::routine::scaffold_prompt(
                &req.intent,
                &crate::routine::RoutineScope::from_legacy_string(&req.scope),
            ),
            authored_by: "scaffold".to_string(),
        })
    };
    // The step runs on the active project's backend. An `Api` project with no key is REFUSED
    // (never run on the subscription); the form still gets the deterministic scaffold, which
    // is labelled `authored_by: scaffold` so it is never mistaken for a model draft.
    let Ok(llm) = state.step_llm() else {
        return scaffold();
    };
    // The model the form picked, else the open project's setting (never the environment).
    let model = step_model_or(
        &state,
        crate::project::StepKind::ResearchChat,
        Some(&req.model),
    );
    // A client engagement never sends client context to a non-Anthropic model: the form gets
    // the scaffold and no model is called.
    if state.client_lock_step_model_refusal(&model).is_some() {
        return scaffold();
    }
    let llm = std::sync::Arc::new(llm);
    let completer = crate::llm::build_completer(
        &model,
        &state.model_registry,
        state.credential_store.as_ref(),
        llm.clone(),
        state.rate_limiter.clone(),
        &state.settings.provider_policy(),
    )
    .unwrap_or(llm);
    match completer
        .complete(
            crate::llm::LlmRequest::new(user)
                .with_model(model)
                .with_system(system),
        )
        .await
    {
        Ok(resp) if !resp.text.trim().is_empty() => Json(crate::routine::DraftPromptResp {
            prompt: resp.text,
            authored_by: "claude".to_string(),
        }),
        _ => scaffold(),
    }
}

/// List available routine templates (preset configurations for common automation patterns).
pub(crate) async fn list_routine_templates() -> Json<Vec<crate::routine::RoutineTemplate>> {
    Json(crate::routine::builtin_templates())
}

/// Instantiate a routine from a template: given a template id, return a fresh Routine
/// prefilled with the template's defaults, ready for the architect to review and customize.
/// The routine is NOT yet persisted; the caller (UI form) passes it to the normal create
/// flow if the architect approves.
pub(crate) async fn instantiate_routine_from_template(
    Path(template_id): Path<String>,
) -> Result<Json<crate::routine::Routine>, AppError> {
    let templates = crate::routine::builtin_templates();
    let template = templates
        .iter()
        .find(|t| t.id == template_id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("template not found: {template_id}")))?;
    Ok(Json(crate::routine::instantiate_from_template(template)))
}

/// Enable or disable a routine.
pub(crate) async fn set_routine_enabled(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<SetEnabledReq>,
) -> Result<Json<Routine>, AppError> {
    state
        .routines
        .set_enabled(&id, req.enabled)
        .map(Json)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("routine not found: {id}")))
}

/// Provision a routine on this backend (the "Set up" action for one that arrived via a
/// project import). Registers it with the scheduler; does not enable it — the architect
/// still presses Start.
pub(crate) async fn provision_routine(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Routine>, AppError> {
    state
        .routines
        .set_provisioned(&id)
        .map(Json)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("routine not found: {id}")))
}

/// Run a routine now (a governed run via the real gate; records the summary). If the run
/// is blocked (gate denials), raise a human-review escalation — same hook the scheduler
/// uses, so a blocked routine surfaces a review whether it fired on a timer or by hand.
pub(crate) async fn run_routine_now(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Routine>, AppError> {
    let routine = state
        .routines
        .run_now(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("routine not found: {id}")))?;
    // If the run blocked, raise a review and link it to this run's history entry.
    if let Some(esc_id) = crate::escalation::raise_if_blocked(&state.escalations, &routine) {
        let _ = state.routines.link_last_run_escalation(&id, &esc_id);
    }
    Ok(Json(routine))
}

/// `GET /api/routines/:id/runs` — the routine's bounded run history, newest first.
pub(crate) async fn routine_runs(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Vec<crate::routine::RoutineRun>>, AppError> {
    state
        .routines
        .runs(&id)
        .map(Json)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("routine not found: {id}")))
}

/// Query for `GET /api/escalations`: `?open=true` returns only open reviews.
#[derive(serde::Deserialize)]
pub(crate) struct EscalationListQuery {
    #[serde(default)]
    pub(crate) open: bool,
}

/// List escalations (all, or only open ones with `?open=true`).
pub(crate) async fn list_escalations(
    State(state): State<AppState>,
    axum::extract::Query(q): axum::extract::Query<EscalationListQuery>,
) -> Json<Vec<crate::escalation::Escalation>> {
    if q.open {
        Json(state.escalations.list_open())
    } else {
        Json(state.escalations.list())
    }
}

/// Raise an escalation against a routine (deduped per routine). The routine's display name
/// is denormalized in from the routine store.
pub(crate) async fn raise_escalation(
    State(state): State<AppState>,
    Json(req): Json<crate::escalation::RaiseEscalationReq>,
) -> Result<Json<crate::escalation::Escalation>, AppError> {
    let name = state
        .routines
        .list()
        .into_iter()
        .find(|r| r.id == req.routine_id)
        .map(|r| r.name)
        .ok_or_else(|| {
            AppError::not_found(anyhow::anyhow!("routine not found: {}", req.routine_id))
        })?;
    Ok(Json(state.escalations.raise_deduped(req, &name)))
}

/// Body for a turn in the escalation review conversation: the human's message + the model
/// the lead-engineer agent should answer on (blank -> server default).
#[derive(serde::Deserialize)]
pub(crate) struct ChatEscalationReq {
    pub(crate) message: String,
    #[serde(default)]
    pub(crate) model: String,
}

/// One turn of the human <-> lead-engineer review conversation. The agent is grounded on
/// the escalation and is instructed NOT to act — only `answer` (authorization) unblocks.
/// Persists both the human message and the reply, and returns the updated escalation.
pub(crate) async fn chat_escalation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ChatEscalationReq>,
) -> Result<Json<crate::escalation::Escalation>, AppError> {
    let esc = state
        .escalations
        .get(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("escalation not found: {id}")))?;
    // GROUNDING (the invariant): the lead-engineer review chat reasons about the ACTUAL
    // project — append the active project's repo + rule digest to the system prompt.
    let system = match state.project_grounding().await {
        Some(grounding) => format!(
            "{}\n\n{grounding}",
            crate::escalation::chat_system_prompt(&esc)
        ),
        None => crate::escalation::chat_system_prompt(&esc),
    };
    let user = crate::escalation::chat_user_prompt(&esc, &req.message);
    // The review chat is grounded in the active project, so it runs on THAT project's backend
    // (the chat setting when there is none), not on the keyless CLI floor; `Api` with no key
    // refuses rather than downgrading.
    let chat_llm = state.step_llm().map_err(backend_refused)?;
    // The model the chat picked, else the open project's escalation model (never the environment).
    let model = step_model_or(
        &state,
        crate::project::StepKind::Escalation,
        Some(&req.model),
    );
    if let Some(why) = state.client_lock_step_model_refusal(&model) {
        return Err(backend_refused(why));
    }
    let completer = crate::llm::build_completer(
        &model,
        &state.model_registry,
        state.credential_store.as_ref(),
        std::sync::Arc::new(chat_llm.clone()),
        state.rate_limiter.clone(),
        &state.settings.provider_policy(),
    )
    .unwrap_or_else(|_| std::sync::Arc::new(chat_llm));
    let reply = match completer
        .complete(
            crate::llm::LlmRequest::new(user)
                .with_model(model)
                .with_system(system),
        )
        .await
    {
        Ok(r) if !r.text.trim().is_empty() => r.text,
        _ => "I couldn't reach the model just now. You can still authorize a decision \
              below, or try again."
            .to_string(),
    };
    state
        .escalations
        .append_turn(&id, &req.message, &reply)
        .map(Json)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("escalation not found: {id}")))
}

/// Resolve an escalation with the human's answer. The answer is run through the
/// AI-translation step (issue #43) — the lead-engineer agent restates it as a precise,
/// structured resume payload on the model the caller selects — and stored on the (now
/// resolved) escalation. On any model failure it falls back to the deterministic scaffold,
/// so resolving never dead-ends. The blocked routine is returned to `Idle` so its next slot
/// can run.
pub(crate) async fn answer_escalation(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<crate::escalation::AnswerEscalationReq>,
) -> Result<Json<serde_json::Value>, AppError> {
    let esc = state
        .escalations
        .get(&id)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("escalation not found: {id}")))?;
    if esc.status != crate::escalation::EscalationStatus::Open {
        // The escalation EXISTS but is already resolved: a state conflict, not a missing
        // resource. 409 (not 404), so a caller can distinguish "gone" from "already answered".
        return Err(AppError::with_status(
            StatusCode::CONFLICT,
            anyhow::anyhow!("no open escalation: {id}"),
        ));
    }
    // Translate the human answer into a structured resume payload via the real LLM seam
    // (model-selectable, with a deterministic fallback inside translate_answer_ai).
    let driver = crate::escalation::LlmTranslator {
        llm: state.step_llm().map_err(backend_refused)?,
        // MULTI-REPO READ: let the translator read across ALL the active project's repo clones.
        repo_dirs: state.active_repo_dirs(),
    };
    // Escalation translation is a NON-FLEET step: the model comes from the active project's
    // per-step config (no env/const fallback once a project exists), replacing the prior
    // per-routine model. DEFAULT_MODEL floor only applies with no active project.
    let model = step_model(&state, crate::project::StepKind::Escalation);
    let grounding = state.project_grounding().await;
    let payload = crate::escalation::translate_answer_ai(
        &driver,
        &esc,
        &req.answer,
        &model,
        grounding.as_deref(),
    )
    .await;
    let resolved = state
        .escalations
        .resolve_with_payload(&id, &req.answer, &payload)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("no open escalation: {id}")))?;
    // Act on the resolution by subject. A ROUTINE review returns the routine to Idle so the
    // scheduler can run its next slot. A UoW (Governed Development) review RESUMES the paused run
    // from its checkpoint (Approve/Amend) — or, on Reject, reverts the worktree and stops cleanly.
    // On a successful Approve/Amend resume we capture the NEW run id (surfaced to the client) and
    // mark the paused run terminal; a resume that cannot start is a 409, not a silent success.
    let mut resume_run_id: Option<String> = None;
    match resolved.subject_kind {
        crate::escalation::SubjectKind::Routine => {
            let _ = state
                .routines
                .set_status(&resolved.routine_id, crate::routine::RoutineStatus::Idle);
        }
        crate::escalation::SubjectKind::Uow => {
            if let Some(ckpt) = resolved
                .checkpoint_id
                .as_deref()
                .and_then(|cid| state.checkpoints.get(cid))
            {
                // The agent-facing directive is the translated payload (Amend carries the
                // correction); fall back to the raw answer if translation produced nothing.
                let directive = resolved
                    .resume_payload
                    .as_ref()
                    .map(|p| p.directive.clone())
                    .filter(|d| !d.trim().is_empty())
                    .unwrap_or_else(|| req.answer.clone());
                // Phase H2: durable record of the human's answer, keyed on the PAUSED run's
                // id (the checkpoint is the only place a UoW escalation carries one — a
                // Routine-subject escalation has no run_id and is out of scope for this
                // per-run audit trail; see the module note).
                state
                    .record_governance(
                        camerata_persistence::GovernanceEvent::info(
                            ckpt.run_id.clone(),
                            "escalation_answered",
                            "human",
                        )
                        .with_story_id(resolved.routine_id.clone())
                        .with_reason(format!("{:?}: {}", req.action, req.answer)),
                    )
                    .await;
                match req.action {
                    crate::escalation::EscalationAction::Reject => {
                        // Stop cleanly: discard the agent's uncommitted work, consume the
                        // checkpoint, and record the abandonment on the UoW (routine_id holds the
                        // story_id for a UoW escalation).
                        let _ = state.checkpoints.mark_resumed(&ckpt.id);
                        // LIFECYCLE-12: reset --hard to the checkpoint's base_commit FIRST so the
                        // per-iteration snapshot COMMITS are reverted too (not just uncommitted
                        // changes), matching the UI's "revert the agent's work" promise.
                        revert_worktree(&ckpt.worktree_dir, Some(&ckpt.base_commit)).await;
                        state.uow.append_history(
                            &resolved.routine_id,
                            "dev_implement",
                            &format!(
                                "Run REJECTED at review; reset the worktree to its base commit \
                                 ({base}) — the agent's committed snapshots and uncommitted \
                                 changes are fully reverted. {directive}",
                                base = ckpt.base_commit,
                            ),
                        );
                    }
                    _ => {
                        // Approve / Amend: re-spawn from the checkpoint with the directive.
                        // A failed resume must NOT read as a successful resolution — surface it
                        // as a 409 (the escalation is already resolved in the store, but the
                        // client learns the run did not restart).
                        match resume_governed_run(&state, &ckpt, &directive).await {
                            Ok((new_run_id, _mode)) => {
                                // The paused run is superseded by the resume run; close it out.
                                state.runs.mark_done(&ckpt.run_id);
                                resume_run_id = Some(new_run_id);
                            }
                            Err(reason) => {
                                return Err(AppError::with_status(
                                    StatusCode::CONFLICT,
                                    anyhow::anyhow!(
                                        "could not resume run from checkpoint: {reason}"
                                    ),
                                ));
                            }
                        }
                    }
                }
            }
        }
    }
    // Return the resolved escalation, annotated with the resumed run id (if any) so the client
    // can follow the new run. Serialise to a Value so we can add the field without changing the
    // persisted Escalation schema.
    let mut body = serde_json::to_value(&resolved).map_err(|e| AppError(e.into()))?;
    if let (Some(rid), Some(obj)) = (resume_run_id, body.as_object_mut()) {
        obj.insert("resume_run_id".to_string(), serde_json::json!(rid));
    }
    Ok(Json(body))
}

/// Edit an existing routine (name / schedule / intent / prompt / scope).
pub(crate) async fn update_routine(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<CreateRoutineReq>,
) -> Result<Json<Routine>, AppError> {
    state
        .routines
        .update(&id, &req)
        .map(Json)
        .ok_or_else(|| AppError::not_found(anyhow::anyhow!("routine not found: {id}")))
}

/// Delete a routine.
pub(crate) async fn delete_routine(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    if state.routines.delete(&id) {
        Ok(Json(serde_json::json!({ "deleted": id })))
    } else {
        Err(AppError::not_found(anyhow::anyhow!(
            "routine not found: {id}"
        )))
    }
}

// ── AI: model provider ────────────────────────────────────────────────────────
