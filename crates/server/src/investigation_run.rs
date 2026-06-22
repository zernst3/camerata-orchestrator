//! The single-agent INVESTIGATION runner (UoW governed-dev redesign, Increment 1).
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
//! # The gate is preserved, universally
//!
//! The investigation agent is built from the SAME [`camerata_fleet::governed_role`] +
//! [`camerata_agent::prepare_session`] machinery the fleet uses, so it carries the
//! identical `--allowedTools` = gated tools only and the identical `--disallowedTools`
//! denylist (`Task`, `Write`, `Bash`, …). The agent's only mutation path is the
//! governance gate; it cannot spawn sub-agents (`Task` is disallowed). Spawning is the
//! server's job, never the agent's.
//!
//! # Token-free fallback
//!
//! When live mode is off (the default; `CAMERATA_LIVE_BUILD != 1`), no `claude` process
//! is spawned: the runner records an honest "investigation pending (live mode off)" note
//! and marks the run AwaitingQa. This keeps CI token-free while the real path is the
//! operator's, exactly mirroring the development run's scripted/live split. Nothing is
//! faked: the token-free path emits a clearly-labelled placeholder note, never invented
//! findings.

use std::sync::atomic::{AtomicUsize, Ordering};

use camerata_agent::prepare_session;
use camerata_core::AgentDriver;
use camerata_fleet::{governed_role, locate_gateway_bin};
use camerata_worktracker::investigation::InvestigationArtifact;

use crate::run::{live_mode_enabled, GateEvent, RunStatus, RunStore};
use crate::uow::UowStore;

/// Run a single gated investigation agent for a story and record its analysis.
///
/// `model` pins the model id for the `claude -p` agent. The caller resolves the
/// default (the active project's `tier_map.strongest`) before calling, so an empty
/// `model` here simply lets the CLI pick its default.
///
/// The run walks: Executing → (agent analyzes) → record note onto the UoW → AwaitingQa.
/// Poll `GET /api/runs/:id` (+ `/agents` once transcripts are wired) to watch it.
pub async fn execute_investigation_run(
    runs: RunStore,
    uow: UowStore,
    run_id: String,
    story_id: String,
    story_title: String,
    story_desc: String,
    model: String,
) {
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

    // ── Live path: one real gated agent ──────────────────────────────────────
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
                },
            );
            runs.set_status(&run_id, RunStatus::AwaitingQa, true);
            return;
        }
    };

    let session_dir =
        std::env::temp_dir().join(format!("camerata-investigation-{}-{}", std::process::id(), run_id));
    // No worktree jail: investigation is read-oriented. The agent's write path is still
    // the gateway only (Task/Write/Bash disallowed by the driver). prepare_session wires
    // the gated MCP config; with no worktree the agent inherits the orchestrator cwd for
    // read scope.
    let spawn = match prepare_session(&session_dir, &gateway_bin, &role, None) {
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
                },
            );
            runs.set_status(&run_id, RunStatus::AwaitingQa, true);
            return;
        }
    };

    let driver = spawn.driver.with_model(&model);

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
        },
    );

    let task = investigation_prompt(&story_id, &story_title, &story_desc);

    match driver.run(&role, &task).await {
        Ok(outcome) => {
            // Record the agent's analysis verbatim as the investigation note. This is
            // honest: the note IS the model's output, attributed to the AI, awaiting the
            // architect's review (note.reviewed = false). No seeded/synthetic content.
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
                },
            );
        }
    }

    runs.set_status(&run_id, RunStatus::AwaitingQa, true);
}

/// Build the investigation agent's task prompt. Read-oriented: it asks the agent to
/// ANALYZE the story and surface decisions/tradeoffs, NOT to write code. Pure + testable.
pub fn investigation_prompt(story_id: &str, title: &str, desc: &str) -> String {
    format!(
        "You are the INVESTIGATION agent for story `{story_id}`. Your job is to ANALYZE, \
         not to build. Do NOT write or scaffold any code.\n\n\
         Story title: {title}\n\
         Story description: {desc}\n\n\
         Read the relevant context and produce a concise investigation note in Markdown that:\n\
         1. Restates what the story asks for in your own words.\n\
         2. Lists the ambiguities and open questions.\n\
         3. Surfaces the concrete DECISIONS / tradeoffs the architect must resolve before \
            any code is written (for each: the question, the options, and your recommended \
            option with reasoning).\n\
         4. States explicitly what is OUT of scope.\n\n\
         Output ONLY the investigation note. The architect reviews it and records the \
         decisions; no code is written until those decisions are approved."
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn investigation_prompt_is_read_oriented_and_names_the_story() {
        let p = investigation_prompt("CAM-7", "Add export", "Members CSV export.");
        assert!(p.contains("CAM-7"));
        assert!(p.contains("Add export"));
        assert!(p.contains("Members CSV export."));
        // It must instruct analysis, NOT code-writing.
        assert!(p.contains("ANALYZE"));
        assert!(p.to_lowercase().contains("do not write"));
        assert!(p.contains("DECISIONS"));
    }

    #[tokio::test]
    async fn investigation_run_token_free_records_placeholder_note_and_completes() {
        // Live mode off (default in tests): no agent spawned, honest placeholder note,
        // run completes to AwaitingQa. This is the CI-safe path.
        std::env::remove_var("CAMERATA_LIVE_BUILD");
        let runs = RunStore::new();
        let uow = UowStore::new();
        let run_id = runs.create("CAM-INV-1", "live");
        execute_investigation_run(
            runs.clone(),
            uow.clone(),
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
    }
}
