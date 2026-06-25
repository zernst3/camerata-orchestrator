//! The single-agent INVESTIGATION runner (UoW governed-dev redesign, Increment 1) with
//! the Phase 3b clarification pause/resume channel.
//!
//! "Begin investigation" kicks ONE gated `claude -p` agent that ANALYZES a story /
//! issue and surfaces the decisions and tradeoffs the architect must resolve before
//! any code is written. It is read-oriented: it does not scaffold a worktree or write
//! code. It is NOT the development fleet — investigation analyzes, development builds.
//!
//! # Why a real, single agent (not the fleet)
//!
//! The development fleet (`live_fleet::execute_live_run`) scaffolds a crate and spawns
//! one governed agent per plan task to WRITE code. Investigation has a different job:
//! read the issue, reason about ambiguities, and emit an investigation note plus
//! proposed decision records. So this runner spawns exactly one agent on one model and
//! records its analysis onto the UoW.
//!
//! # The gate is preserved, universally — AND ask_clarification does not weaken it
//!
//! The investigation agent is built from the SAME [`camerata_fleet::governed_role`] +
//! [`camerata_agent::prepare_session`] machinery the fleet uses, so it carries the
//! identical `--allowedTools` = gated tools only and the identical `--disallowedTools`
//! denylist (`Task`, `Write`, `Bash`, …). The agent's only mutation path is the
//! governance gate; it cannot spawn sub-agents (`Task` is disallowed). Spawning is the
//! server's job, never the agent's.
//!
//! Phase 3b adds the READ-CLASS `ask_clarification` tool to this agent's allowlist via
//! [`camerata_agent::ClaudeCliDriver::with_clarification`]. That tool RECORDS a structured
//! question to a per-session sink; it does NOT write to the repo, spawn, or escalate, so
//! it adds NO new write path and the deny-before-write gate is unchanged.
//!
//! # The pause/resume channel (Phase 3b)
//!
//! A blocking long-poll (the subprocess waiting hours for a human) would hang/timeout, so
//! instead: when the agent calls `ask_clarification`, the gateway records the question to
//! `<session_dir>/clarify-requests.jsonl` and tells the agent to STOP. After the agent
//! returns, the server reads that sink. If a question was raised it: posts it into the 3a
//! [`crate::clarify::ClarificationStore`] (auto-saved), persists a resume context, and
//! parks the run at [`RunStatus::AwaitingClarification`]. When the human answers via the
//! existing 3a endpoint, [`resume_investigation_after_clarification`] re-spawns the SAME
//! gated agent (same governed role, same gate) with the original task + the question + the
//! answer appended, so it continues.
//!
//! # Token-free fallback
//!
//! When live mode is off (the default; `CAMERATA_LIVE_BUILD != 1`), no `claude` process
//! is spawned: the runner records an honest "investigation pending (live mode off)" note
//! and marks the run AwaitingQa. This keeps CI token-free while the real path is the
//! operator's, exactly mirroring the development run's scripted/live split. Nothing is
//! faked: the token-free path emits a clearly-labelled placeholder note, never invented
//! findings.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use camerata_agent::{HeartbeatFn, prepare_session};
use camerata_core::AgentDriver;
use camerata_fleet::{governed_role, locate_gateway_bin};
use camerata_worktracker::investigation::InvestigationArtifact;
use serde::Deserialize;

use crate::clarify::{ClarificationStore, ClarifyOption};
use crate::clarify_resume::{ClarifyResumeContext, ClarifyResumeStore, PausedPhase};
use crate::run::{live_mode_enabled, GateEvent, RunStatus, RunStore};
use crate::uow::UowStore;

/// The wire shape of one clarification request the gateway's `ask_clarification` tool
/// appends to `<session_dir>/clarify-requests.jsonl`. Mirrors the gateway binary's
/// `ClarificationRequestRecord` (the binary's type is not importable as a lib type), so
/// the server can read questions back off the agent→run channel.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ClarifyRequestRecord {
    question: String,
    #[serde(default)]
    options: Vec<ClarifyRequestOptionRecord>,
    #[serde(default)]
    multi_select: bool,
    #[serde(default = "default_true")]
    allow_free_text: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ClarifyRequestOptionRecord {
    label: String,
    #[serde(default)]
    description: String,
}

fn default_true() -> bool {
    true
}

/// Read the FIRST clarification the agent raised from the session's clarify-request sink.
///
/// The agent is instructed to ask ONE question then stop; if it raised more than one we
/// honour the first (the most blocking decision) — the rest can be re-raised on resume.
/// Returns `None` when the sink is absent/empty/unparseable (the common case: the agent
/// did not need to ask). Pure file read; no side effects.
pub(crate) fn read_first_clarify_request(session_dir: &Path) -> Option<ClarifyRequestRecord> {
    let sink = session_dir.join("clarify-requests.jsonl");
    let text = std::fs::read_to_string(sink).ok()?;
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .find_map(|l| serde_json::from_str::<ClarifyRequestRecord>(l).ok())
}

/// Build the investigation agent's task prompt for a FRESH run. Read-oriented: it asks
/// the agent to ANALYZE the story and surface decisions/tradeoffs, NOT to write code, and
/// tells it to use `ask_clarification` for any blocking product decision rather than
/// guessing. Pure + testable.
pub fn investigation_prompt(
    story_id: &str,
    title: &str,
    desc: &str,
    grounding: Option<&str>,
) -> String {
    // GROUNDING (the invariant): even though this agent can READ the repo clone directly,
    // give it the project's rule context + a repo summary up front, and tell it to consult
    // the actual code. See docs/decisions/2026-06-25_all-agents-grounded-in-repo-and-rules.md.
    let grounding_block = match grounding {
        Some(g) if !g.trim().is_empty() => format!(
            "{}\n\nGround your analysis in the project facts above AND in the ACTUAL repo \
             code you can read from the working directory — do not assume capabilities the \
             stack/dependencies don't show.\n\n",
            g.trim()
        ),
        _ => String::new(),
    };
    format!(
        "You are the INVESTIGATION agent for story `{story_id}`. Your job is to ANALYZE, \
         not to build. Do NOT write or scaffold any code.\n\n\
         {grounding_block}\
         Story title: {title}\n\
         Story description: {desc}\n\n\
         Read the relevant context and produce a concise investigation note in Markdown that:\n\
         1. Restates what the story asks for in your own words.\n\
         2. Lists the ambiguities and open questions.\n\
         3. Surfaces the concrete DECISIONS / tradeoffs the architect must resolve before \
            any code is written (for each: the question, the options, and your recommended \
            option with reasoning).\n\
         4. States explicitly what is OUT of scope.\n\n\
         If a SINGLE product/design decision genuinely BLOCKS your analysis and you cannot \
         make it yourself, call the `ask_clarification` tool with a structured question \
         (options each with a benefit/drawback) instead of guessing, then STOP — you will \
         be resumed with the human's answer.\n\n\
         Output ONLY the investigation note. The architect reviews it and records the \
         decisions; no code is written until those decisions are approved."
    )
}

/// Build the RESUME task prompt: the original task plus the asked question plus the
/// human's answer, so the re-spawned agent has the full prior context (we re-spawn fresh
/// rather than long-poll a hung subprocess). Pure + testable. This is the proof point that
/// resume "builds the agent context including the Q+A".
pub fn investigation_resume_prompt(original_task: &str, question: &str, answer: &str) -> String {
    format!(
        "{original_task}\n\n\
         ── RESUMING AFTER CLARIFICATION ──\n\
         Earlier you asked: \"{question}\"\n\
         The human answered: \"{answer}\"\n\n\
         Continue your investigation with this answer settled. Do not re-ask it. Produce \
         the investigation note. If a DIFFERENT blocking decision arises you may call \
         `ask_clarification` again and stop; otherwise finish."
    )
}

/// Run a single gated investigation agent for a story and record its analysis.
///
/// `model` pins the model id for the `claude -p` agent. The caller resolves the
/// default (the active project's `tier_map.strongest`) before calling, so an empty
/// `model` here simply lets the CLI pick its default.
///
/// The run walks: Executing → (agent analyzes) → record note onto the UoW → AwaitingQa,
/// OR Executing → (agent asks) → pause at AwaitingClarification (Phase 3b).
/// Poll `GET /api/runs/:id` (+ `/agents` once transcripts are wired) to watch it.
#[allow(clippy::too_many_arguments)]
pub async fn execute_investigation_run(
    runs: RunStore,
    uow: UowStore,
    clarifications: ClarificationStore,
    resume: ClarifyResumeStore,
    run_id: String,
    story_id: String,
    story_title: String,
    story_desc: String,
    model: String,
    grounding: Option<String>,
) {
    // Honor a cancel that arrived before the executor got scheduled: leave the run in its
    // terminal Cancelled state (set by RunStore::cancel) and do nothing.
    if runs.is_cancelled(&run_id) {
        return;
    }

    runs.set_status(&run_id, RunStatus::Executing, false);
    let seq = AtomicUsize::new(0);
    let next_seq = || seq.fetch_add(1, Ordering::SeqCst) + 1;

    if !live_mode_enabled() {
        // Token-free default: no agent spawned. Record an honest, clearly-labelled
        // placeholder so the timeline reflects that investigation was started but the
        // live agent did not run. No invented findings.
        runs.push_event(
            &run_id,
            GateEvent {
                seq: next_seq(),
                layer: "investigation".to_string(),
                verdict: "info".to_string(),
                rule: None,
                detail: "Investigation started (live mode off): no agent spawned. \
                         Set CAMERATA_LIVE_BUILD=1 to run the real single-agent analysis."
                    .to_string(),
                content_hash: None,
            },
        );
        let note = InvestigationArtifact::ai_authored(
            &story_id,
            "Investigation pending — live mode is off, so no analysis agent ran. \
             Enable CAMERATA_LIVE_BUILD=1 and re-run to produce a real investigation note.",
            chrono::Utc::now(),
        );
        uow.set_investigation_note(&note);
        uow.append_history(
            &story_id,
            "note",
            "Investigation run started (live mode off; placeholder note recorded).",
        );
        runs.set_status(&run_id, RunStatus::AwaitingQa, true);
        return;
    }

    // Live path: build the agent + run one analysis pass with the fresh prompt.
    let task = investigation_prompt(&story_id, &story_title, &story_desc, grounding.as_deref());
    run_one_investigation_pass(
        runs,
        uow,
        clarifications,
        resume,
        run_id,
        story_id,
        story_title,
        story_desc,
        model,
        task,
        next_seq,
    )
    .await;
}

/// Resume an investigation run that was PAUSED on a clarification, now that the human has
/// answered. Re-spawns the SAME gated agent (same governed role, gate intact) with the
/// original task + the asked question + the answer appended, and runs another pass.
///
/// The resume context was persisted at the pause point and is CONSUMED here (so a run
/// cannot be double-resumed). The clarify question itself is already marked answered by
/// the 3a store before this is called.
pub async fn resume_investigation_after_clarification(
    runs: RunStore,
    uow: UowStore,
    clarifications: ClarificationStore,
    resume: ClarifyResumeStore,
    ctx: ClarifyResumeContext,
    answer_summary: String,
) {
    let seq = AtomicUsize::new(usize::MAX / 2); // resume events sort after the originals
    let next_seq = || seq.fetch_add(1, Ordering::SeqCst) + 1;

    runs.set_status(&ctx.run_id, RunStatus::Executing, false);
    runs.push_event(
        &ctx.run_id,
        GateEvent {
            seq: next_seq(),
            layer: "clarification".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: format!(
                "Answer received (\"{answer_summary}\"). Resuming the gated investigation \
                 agent with the answer in context.",
            ),
            content_hash: None,
        },
    );

    let task = investigation_resume_prompt(&ctx.original_task, &ctx.asked_question, &answer_summary);
    run_one_investigation_pass(
        runs,
        uow,
        clarifications,
        resume,
        ctx.run_id,
        ctx.story_id,
        ctx.story_title,
        ctx.story_desc,
        ctx.model,
        task,
        next_seq,
    )
    .await;
}

/// PAUSE a run on a raised clarification (Phase 3b checkpoint): post the question into the
/// 3a clarify store (reused AS-IS, auto-saved), persist the resume context so the run can
/// re-spawn on answer (survives a restart), record the pause event, and park the run at
/// [`RunStatus::AwaitingClarification`] (NOT done). This is the auto-save checkpoint; it is
/// extracted from the spawn pass so it is unit-testable token-free.
#[allow(clippy::too_many_arguments)]
pub(crate) fn pause_run_on_clarification(
    runs: &RunStore,
    uow: &UowStore,
    clarifications: &ClarificationStore,
    resume: &ClarifyResumeStore,
    run_id: &str,
    story_id: &str,
    story_title: &str,
    story_desc: &str,
    model: &str,
    task: &str,
    req: ClarifyRequestRecord,
    seq: usize,
) {
    let options: Vec<ClarifyOption> = req
        .options
        .into_iter()
        .map(|o| ClarifyOption {
            label: o.label,
            description: o.description,
        })
        .collect();
    // Reuse the 3a store + model AS-IS — addressee "you" (the architect).
    let clar = clarifications.post_structured(
        story_id,
        &req.question,
        "you",
        options,
        req.multi_select,
        req.allow_free_text,
    );
    // Persist enough to re-spawn on answer (survives a restart).
    resume.put(
        &clar.id,
        ClarifyResumeContext {
            run_id: run_id.to_string(),
            story_id: story_id.to_string(),
            story_title: story_title.to_string(),
            story_desc: story_desc.to_string(),
            model: model.to_string(),
            phase: PausedPhase::Investigation,
            original_task: task.to_string(),
            asked_question: req.question.clone(),
        },
    );
    uow.append_history(
        story_id,
        "note",
        "Investigation agent raised a clarifying question; run paused for an answer.",
    );
    runs.push_event(
        run_id,
        GateEvent {
            seq,
            layer: "clarification".to_string(),
            verdict: "pause".to_string(),
            rule: None,
            detail: format!(
                "The investigation agent needs a decision: \"{}\". This run is waiting on \
                 you — answer it to resume. ({})",
                req.question, clar.id
            ),
            content_hash: None,
        },
    );
    // Parked, NOT done: the run resumes when the clarification is answered.
    runs.set_status(run_id, RunStatus::AwaitingClarification, false);
}

/// Build a [`ClarifyRequestRecord`] from its parts. Test-only constructor (the struct's
/// fields are private and the live path builds it via serde from the gateway sink).
#[cfg(test)]
pub(crate) fn clarify_request_for_test(
    question: &str,
    options: Vec<(&str, &str)>,
    multi_select: bool,
    allow_free_text: bool,
) -> ClarifyRequestRecord {
    ClarifyRequestRecord {
        question: question.to_string(),
        options: options
            .into_iter()
            .map(|(l, d)| ClarifyRequestOptionRecord {
                label: l.to_string(),
                description: d.to_string(),
            })
            .collect(),
        multi_select,
        allow_free_text,
    }
}

/// One spawn-and-handle pass shared by the fresh run and the resume. Spawns ONE gated
/// investigation agent on `task`, then either pauses on a raised clarification (Phase 3b)
/// or records the note and completes. `next_seq` is the run's event sequencer.
#[allow(clippy::too_many_arguments)]
async fn run_one_investigation_pass(
    runs: RunStore,
    uow: UowStore,
    clarifications: ClarificationStore,
    resume: ClarifyResumeStore,
    run_id: String,
    story_id: String,
    story_title: String,
    story_desc: String,
    model: String,
    task: String,
    next_seq: impl Fn() -> usize,
) {
    let gateway_bin = match locate_gateway_bin() {
        Ok(bin) => bin,
        Err(e) => {
            runs.push_event(
                &run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "setup".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: format!(
                        "Investigation needs the gateway binary: {e}. Build it with \
                         `cargo build -p camerata-gateway`, then retry."
                    ),
                    content_hash: None,
                },
            );
            runs.set_status(&run_id, RunStatus::AwaitingQa, true);
            return;
        }
    };

    // Build the SAME governed role the fleet uses (every enforced gate rule in force),
    // and prepare ONE gated session. This is what makes the investigation agent carry
    // the identical universal tool gate: allowedTools = gated tools only, Task disallowed.
    let role = match governed_role("Investigator").await {
        Ok(r) => r,
        Err(e) => {
            runs.push_event(
                &run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "setup".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: format!("Could not build the governed investigator role: {e}"),
                    content_hash: None,
                },
            );
            runs.set_status(&run_id, RunStatus::AwaitingQa, true);
            return;
        }
    };

    // No worktree jail: investigation is read-oriented. The agent's write path is still
    // the gateway only (Task/Write/Bash disallowed by the driver). prepare_session wires
    // the gated MCP config; with no worktree the agent inherits the orchestrator cwd for
    // read scope.
    // The session temp dir is RAII-managed inside SessionSpawn._dir (ARCH-RESOURCE-LIFECYCLE-1);
    // a unique dir is created per prepare_session call so a resume's sink never collides.
    let spawn = match prepare_session(&gateway_bin, &role, None) {
        Ok(s) => s,
        Err(e) => {
            runs.push_event(
                &run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "setup".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: format!("Could not prepare the investigation session: {e}"),
                    content_hash: None,
                },
            );
            runs.set_status(&run_id, RunStatus::AwaitingQa, true);
            return;
        }
    };

    // Opt the investigation agent into the READ-CLASS ask_clarification tool (Phase 3b).
    // This adds NO write path: the gate (gated_write only) and the disallowed-builtins
    // denylist (Task/Write/Bash/…) are unchanged.
    // Wire the activity heartbeat so streamed agent output keeps last_activity_ms fresh.
    // Destructure spawn so the _dir (TempDir RAII guard) is held independently while
    // the driver is consumed. The dir stays alive until `_session_dir` is dropped.
    let camerata_agent::SessionSpawn { driver: raw_driver, _dir: _session_dir, .. } = spawn;
    let store_hb = runs.clone();
    let rid_hb = run_id.clone();
    let on_activity: HeartbeatFn = Arc::new(move || store_hb.touch_activity(&rid_hb, None));
    let driver = raw_driver
        .with_model(&model)
        .with_clarification(true)
        .with_on_activity(on_activity);

    runs.push_event(
        &run_id,
        GateEvent {
            seq: next_seq(),
            layer: "investigation".to_string(),
            verdict: "info".to_string(),
            rule: None,
            detail: format!(
                "Spawning single gated investigation agent on model `{}`.",
                if model.trim().is_empty() {
                    "<cli default>"
                } else {
                    model.as_str()
                }
            ),
            content_hash: None,
        },
    );

    // Last cancel check before the (potentially long) agent subprocess. A cancel after
    // this point aborts the spawned task (RunStore::cancel), dropping the driver future
    // and killing the kill_on_drop child — so this guard plus the abort together cover the
    // whole window.
    if runs.is_cancelled(&run_id) {
        return;
    }

    let agent_result = driver.run(&role, &task).await;

    // If a cancel landed while the agent was running (and somehow the task wasn't aborted),
    // do not record a note or advance to AwaitingQa — leave the terminal Cancelled state.
    if runs.is_cancelled(&run_id) {
        return;
    }

    match agent_result {
        Ok(outcome) => {
            // Phase 3b: did the agent raise a clarifying question this pass? If so, PAUSE
            // — post the question into the 3a store (auto-saved), persist the resume
            // context, and park the run at AwaitingClarification. The agent already STOPped
            // (the question was its last act), so there is no hung process to long-poll.
            if let Some(req) = read_first_clarify_request(_session_dir.path()) {
                pause_run_on_clarification(
                    &runs,
                    &uow,
                    &clarifications,
                    &resume,
                    &run_id,
                    &story_id,
                    &story_title,
                    &story_desc,
                    &model,
                    &task,
                    req,
                    next_seq(),
                );
                // spawn is dropped here; _dir (TempDir) removes the session dir automatically.
                return;
            }

            // No question raised: record the analysis verbatim as the investigation note.
            // This is honest: the note IS the model's output, attributed to the AI,
            // awaiting the architect's review (note.reviewed = false). No synthetic content.
            let note = InvestigationArtifact::ai_authored(
                &story_id,
                outcome.result.clone(),
                chrono::Utc::now(),
            );
            uow.set_investigation_note(&note);
            uow.append_history(
                &story_id,
                "note",
                "Investigation agent produced an analysis note (awaiting architect review).",
            );
            runs.push_event(
                &run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "investigation".to_string(),
                    verdict: "allow".to_string(),
                    rule: None,
                    detail: format!(
                        "Investigation note recorded ({} chars). Architect reviews + records decisions next.",
                        outcome.result.len()
                    ),
                    content_hash: None,
                },
            );
        }
        Err(e) => {
            runs.push_event(
                &run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "investigation".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: format!("Investigation agent failed: {e}"),
                    content_hash: None,
                },
            );
        }
    }

    // _session_dir (TempDir) removes the session dir automatically on drop here.
    drop(_session_dir);
    runs.set_status(&run_id, RunStatus::AwaitingQa, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn investigation_prompt_is_read_oriented_and_names_the_story() {
        let p = investigation_prompt("CAM-7", "Add export", "Members CSV export.", None);
        assert!(p.contains("CAM-7"));
        assert!(p.contains("Add export"));
        assert!(p.contains("Members CSV export."));
        // It must instruct analysis, NOT code-writing.
        assert!(p.contains("ANALYZE"));
        assert!(p.to_lowercase().contains("do not write"));
        assert!(p.contains("DECISIONS"));
        // It must point the agent at ask_clarification for blocking decisions.
        assert!(p.contains("ask_clarification"));
    }

    #[test]
    fn resume_prompt_carries_original_task_question_and_answer() {
        let p = investigation_resume_prompt(
            "Analyze story CAM-7.",
            "Include archived members?",
            "No, active only.",
        );
        // The resume context the re-spawned agent sees includes all three — the proof
        // that resume builds the agent context with the Q+A.
        assert!(p.contains("Analyze story CAM-7."));
        assert!(p.contains("Include archived members?"));
        assert!(p.contains("No, active only."));
        assert!(p.contains("RESUMING AFTER CLARIFICATION"));
        assert!(p.to_lowercase().contains("do not re-ask"));
    }

    #[test]
    fn read_first_clarify_request_parses_a_recorded_question() {
        let dir = std::env::temp_dir().join(format!(
            "cam-inv-sink-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Mirror exactly what the gateway's ask_clarification writes.
        let line = serde_json::json!({
            "question": "Which timezone for reminders?",
            "options": [
                {"label": "Org", "description": "one send time"},
                {"label": "Member", "description": "local hour"}
            ],
            "multi_select": false,
            "allow_free_text": true,
            "ts_ms": 12u64
        })
        .to_string();
        std::fs::write(dir.join("clarify-requests.jsonl"), format!("{line}\n")).unwrap();

        let req = read_first_clarify_request(&dir).expect("parses the recorded question");
        assert_eq!(req.question, "Which timezone for reminders?");
        assert_eq!(req.options.len(), 2);
        assert_eq!(req.options[0].label, "Org");
        assert!(req.allow_free_text);
        assert!(!req.multi_select);

        // No sink => None (the common no-question case).
        let empty = std::env::temp_dir().join(format!("cam-inv-empty-{}", std::process::id()));
        std::fs::create_dir_all(&empty).unwrap();
        assert!(read_first_clarify_request(&empty).is_none());

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn investigation_session_gate_posture_is_unchanged_with_clarification_on() {
        // THE GATE NEVER WEAKENS. The investigation runner opts the agent into the
        // READ-CLASS ask_clarification tool via the SAME prepare_session machinery the
        // fleet/update_branch_run use. Assert, at the session-driver level, that enabling
        // it leaves the write gate intact: gated_write is the only write path, and every
        // write/exec/spawn built-in (esp. Task) stays disallowed and off the allowlist.
        use camerata_agent::{prepare_session, ASK_CLARIFICATION_TOOL, GATED_WRITE_TOOL};
        use camerata_core::{Role, RuleId};

        let role = Role {
            name: "Investigator".to_string(),
            rule_subset: vec![RuleId("GOV-1".to_string())],
            allowed_paths: vec!["crates/".to_string()],
        };
        // prepare_session now creates its own RAII TempDir internally (ARCH-RESOURCE-LIFECYCLE-1).
        let spawn = prepare_session(Path::new("/bin/camerata-gateway"), &role, None)
            .expect("session prepares");
        let driver = spawn.driver.with_clarification(true);
        let args = driver.build_args(&role, "analyze");

        let allowed = {
            let i = args.iter().position(|a| a == "--allowedTools").unwrap();
            args[i + 1].clone()
        };
        let disallowed = {
            let i = args.iter().position(|a| a == "--disallowedTools").unwrap();
            args[i + 1].clone()
        };
        // The only write tool is still gated_write; ask_clarification rides alongside it.
        assert!(allowed.split(' ').any(|t| t == GATED_WRITE_TOOL));
        assert!(allowed.split(' ').any(|t| t == ASK_CLARIFICATION_TOOL));
        // Every escape tool stays denied and absent from the allowlist — unchanged.
        for tool in ["Bash", "Write", "Edit", "MultiEdit", "NotebookEdit", "Task"] {
            assert!(
                disallowed.split(' ').any(|t| t == tool),
                "{tool} must stay on the denylist with clarification on"
            );
            assert!(
                !allowed.split(' ').any(|t| t == tool),
                "{tool} must never be on the allowlist with clarification on"
            );
        }
        // spawn._dir (TempDir) is dropped here, cleaning up the session dir automatically.
    }

    #[test]
    fn pause_posts_question_persists_resume_and_parks_the_run() {
        // The Phase 3b pause checkpoint, token-free: a raised question posts to the 3a
        // store, persists a resume context, and parks the run at AwaitingClarification —
        // and ALL of it survives a reload (the auto-saved resume guarantee).
        let dir = std::env::temp_dir().join(format!(
            "cam-pause-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let clar_path = dir.join("clarifications.json");
        let resume_path = dir.join("clarify-resume.json");

        let runs = RunStore::new();
        let uow = UowStore::new();
        let run_id = runs.create("CAM-7", "investigation", crate::run::RunKind::Watched);

        let parked_clar_id;
        {
            // Persistent stores, like the running server's.
            let clarifications = ClarificationStore::at(clar_path.clone());
            let resume = ClarifyResumeStore::at(resume_path.clone());

            let req = clarify_request_for_test(
                "Include archived members in the export?",
                vec![
                    ("Active only", "simpler, matches the common case"),
                    ("Include archived", "complete, but larger export"),
                ],
                false,
                true,
            );
            pause_run_on_clarification(
                &runs,
                &uow,
                &clarifications,
                &resume,
                &run_id,
                "CAM-7",
                "Add export",
                "Members CSV export.",
                "claude-opus-4-8",
                &investigation_prompt("CAM-7", "Add export", "Members CSV export.", None),
                req,
                1,
            );

            // The run is parked — AwaitingClarification, NOT done.
            let run = runs.get(&run_id).expect("run exists");
            assert_eq!(run.status, RunStatus::AwaitingClarification);
            assert!(!run.done, "a parked run is not done");
            assert!(run
                .events
                .iter()
                .any(|e| e.layer == "clarification" && e.verdict == "pause"));

            // The question is in the 3a store (open), addressee "you".
            let open = clarifications.for_story("CAM-7");
            assert_eq!(open.len(), 1);
            parked_clar_id = open[0].id.clone();
            assert_eq!(open[0].addressee, "you");
            assert_eq!(open[0].options.len(), 2);
            assert!(open[0].is_open());
            // stores dropped here (the process "restarts")
        }

        // Reload BOTH stores from disk — the pause point survived the restart.
        let clarifications = ClarificationStore::at(clar_path.clone());
        let resume = ClarifyResumeStore::at(resume_path.clone());

        // The clarification survived and is still open.
        let restored_q = clarifications
            .for_story("CAM-7")
            .into_iter()
            .find(|c| c.id == parked_clar_id)
            .expect("question survived reload");
        assert!(restored_q.is_open());
        assert_eq!(restored_q.question, "Include archived members in the export?");

        // The resume context survived and carries enough to re-spawn with the Q in context.
        let ctx = resume.get(&parked_clar_id).expect("resume ctx survived reload");
        assert_eq!(ctx.run_id, run_id);
        assert_eq!(ctx.phase, PausedPhase::Investigation);
        assert_eq!(ctx.model, "claude-opus-4-8");
        assert!(ctx.original_task.contains("CAM-7"));
        assert_eq!(ctx.asked_question, "Include archived members in the export?");

        // Now ANSWER it (as the answer endpoint does) and verify the resume path can
        // consume the context exactly once and build a prompt carrying the Q + the answer.
        let answered = clarifications
            .answer(&parked_clar_id, "Active only.", "zach")
            .expect("answerable");
        assert!(!answered.is_open());
        let taken = resume.take(&parked_clar_id).expect("resume ctx present");
        // Consumed exactly once — no double-resume.
        assert!(resume.take(&parked_clar_id).is_none());
        let resume_prompt = investigation_resume_prompt(
            &taken.original_task,
            &taken.asked_question,
            answered.answer.as_deref().unwrap_or_default(),
        );
        assert!(resume_prompt.contains("Include archived members in the export?"));
        assert!(resume_prompt.contains("Active only."));
        assert!(resume_prompt.contains("CAM-7"));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn investigation_run_honors_cancel_before_start() {
        // A cancel that landed before the executor ran leaves the run terminal Cancelled
        // and does NOT advance it to AwaitingQa or record a placeholder note.
        std::env::remove_var("CAMERATA_LIVE_BUILD");
        let runs = RunStore::new();
        let uow = UowStore::new();
        let clarifications = ClarificationStore::new();
        let resume = ClarifyResumeStore::new();
        let run_id = runs.create("CAM-INV-CANCEL", "live", crate::run::RunKind::Watched);

        // Cancel BEFORE the executor runs (simulates a Stop that beat the spawn).
        runs.cancel(&run_id);

        execute_investigation_run(
            runs.clone(),
            uow.clone(),
            clarifications.clone(),
            resume.clone(),
            run_id.clone(),
            "CAM-INV-CANCEL".to_string(),
            "A story".to_string(),
            "Some description.".to_string(),
            "claude-opus-4-8".to_string(),
        )
        .await;

        let run = runs.get(&run_id).expect("run exists");
        assert_eq!(run.status, RunStatus::Cancelled);
        assert!(run.done);
        // No placeholder investigation event was recorded (the executor returned early).
        assert!(!run.events.iter().any(|e| e.detail.contains("live mode off")));
    }

    #[tokio::test]
    async fn investigation_run_token_free_records_placeholder_note_and_completes() {
        // Live mode off (default in tests): no agent spawned, honest placeholder note,
        // run completes to AwaitingQa. This is the CI-safe path.
        std::env::remove_var("CAMERATA_LIVE_BUILD");
        let runs = RunStore::new();
        let uow = UowStore::new();
        let clarifications = ClarificationStore::new();
        let resume = ClarifyResumeStore::new();
        let run_id = runs.create("CAM-INV-1", "live", crate::run::RunKind::Watched);
        execute_investigation_run(
            runs.clone(),
            uow.clone(),
            clarifications.clone(),
            resume.clone(),
            run_id.clone(),
            "CAM-INV-1".to_string(),
            "A story".to_string(),
            "Some description.".to_string(),
            "claude-opus-4-8".to_string(),
        )
        .await;

        let run = runs.get(&run_id).expect("run exists");
        assert_eq!(run.status, RunStatus::AwaitingQa);
        assert!(run.done);
        // The placeholder event is present and clearly labelled as live-mode-off.
        assert!(run
            .events
            .iter()
            .any(|e| e.detail.contains("live mode off")));
        // Token-free path posts no clarification.
        assert!(clarifications.for_story("CAM-INV-1").is_empty());
    }
}
