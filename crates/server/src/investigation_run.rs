//! The single-agent INVESTIGATION runner (UoW governed-dev redesign, Increment 1) with
//! the Phase 3b clarification pause/resume channel.
//!
//! "Begin investigation" kicks ONE gated `claude -p` agent that ANALYZES a story /
//! issue and surfaces the decisions and tradeoffs the architect must resolve before
//! any code is written. It is read-oriented: it does not scaffold a worktree or write
//! code. It is NOT the development fleet — investigation analyzes, development builds.
//!
//! # On-demand full-repo read (the invariant)
//!
//! The agent runs WITH the active project's local clone as its cwd + `--add-dir`, so its
//! read-only built-ins (Read/Grep/Glob/LS) can open ANY file in the repo it is
//! investigating — not just the digest inlined into the prompt. Truth comes from reading
//! the actual code, never from assumptions. This is READ access only; the write path is
//! still `gated_write` (jailed to the same repo), Task/Write/Bash disallowed by the driver.
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
//! # Structured decisions (`propose_decision`)
//!
//! The agent files each decision it surfaces through the READ-CLASS `propose_decision` tool.
//! Like `ask_clarification` it only RECORDS (to `<session_dir>/decisions.jsonl`, outside the
//! worktree jail), so it adds no write path. After the pass the server reads the sink and
//! merges the proposals into the story's decisions as `Pending` AI records
//! ([`merge_proposed_decisions`]); the architect approves, changes or rejects each one. An
//! approved or rejected record is never overwritten.
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
//! can be spawned, so there is no analysis to record. The runner REFUSES honestly: it
//! records a clearly-labelled `error` event, fails the run, and leaves the UoW
//! investigation note UNTOUCHED (it never writes a synthetic "pending" note that could be
//! mistaken for a real artifact). This mirrors the token-free refusals in
//! `dev_implement_run` / `pr_resolve_run` / `update_branch_run`: nothing is faked and no
//! real domain state is advanced on a non-live run.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use camerata_agent::{prepare_session, HeartbeatFn};
use camerata_core::AgentDriver;
use camerata_fleet::{governed_role, locate_gateway_bin};
use camerata_worktracker::investigation::{
    recommended_rationale, slugify_decision_label, DecisionOutcome, DecisionRecord,
    InvestigationArtifact,
};
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

/// The wire shape of one decision the gateway's `propose_decision` tool appends to
/// `<session_dir>/decisions.jsonl`. Mirrors the gateway binary's `ProposedDecisionRecord` (the
/// binary's type is not importable as a lib type). Only `label` is required on the wire; the
/// other fields default so a partial line still parses and is then judged by
/// [`merge_proposed_decisions`].
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct ProposedDecisionRecord {
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) question: String,
    #[serde(default)]
    pub(crate) options: Vec<ProposedOptionRecord>,
    #[serde(default)]
    pub(crate) recommended: String,
    #[serde(default)]
    pub(crate) rationale: String,
}

/// One option of a [`ProposedDecisionRecord`].
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct ProposedOptionRecord {
    pub(crate) label: String,
    #[serde(default)]
    pub(crate) description: String,
}

/// Read EVERY decision the agent proposed this pass from the session's decisions sink, in
/// the order recorded. Blank and malformed lines are skipped. An absent sink is the common
/// case (the agent proposed nothing) and gives an empty list. Pure file read.
pub(crate) fn read_proposed_decisions(session_dir: &Path) -> Vec<ProposedDecisionRecord> {
    let Ok(text) = std::fs::read_to_string(session_dir.join("decisions.jsonl")) else {
        return Vec::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ProposedDecisionRecord>(l).ok())
        .collect()
}

/// The result of merging a pass's proposals into a story's existing decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecisionMerge {
    /// The full decision list to store.
    pub(crate) decisions: Vec<DecisionRecord>,
    /// How many proposals were filed as `Pending` (new, or replacing an older `Pending`).
    pub(crate) proposed: usize,
    /// How many existing records were left exactly as they were.
    pub(crate) kept: usize,
    /// How many proposals were ignored because the architect already ruled on that decision
    /// (an `Approved` or `Rejected` record with the same artifact id).
    pub(crate) ruled_already: usize,
    /// One plain-language line per proposal that was dropped as unusable.
    pub(crate) dropped: Vec<String>,
    /// One plain-language line per proposal that was kept but had oversized parts cut down.
    pub(crate) trimmed: Vec<String>,
}

/// Size caps on what the agent may file (the sink is agent output, so it is bounded before it
/// reaches the decisions store and the console).
const MAX_LABEL_CHARS: usize = 200;
const MAX_TEXT_BYTES: usize = 4096;
const MAX_OPTIONS: usize = 10;
const MAX_OPTION_CHARS: usize = 200;
const MAX_OPTION_DESCRIPTION_CHARS: usize = 500;

/// `s` cut to at most `max` characters; the flag says whether anything was cut.
fn cut_chars(s: &str, max: usize) -> (String, bool) {
    match s.char_indices().nth(max) {
        Some((i, _)) => (s[..i].trim_end().to_string(), true),
        None => (s.to_string(), false),
    }
}

/// `s` cut to at most `max` bytes on a character boundary; the flag says whether anything was cut.
fn cut_bytes(s: &str, max: usize) -> (String, bool) {
    if s.len() <= max {
        return (s.to_string(), false);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    (s[..end].trim_end().to_string(), true)
}

/// Merge the agent's proposals into the story's existing decisions. Pure.
///
/// - The artifact id is `{story_id}/decision/{slug}` with the slug of the label, so the
///   same label in a later pass finds the same record.
/// - An existing `Approved` or `Rejected` record with that id is kept as is: the architect
///   already ruled. An existing `Pending` one is replaced in place by the new proposal.
///   Existing records no proposal mentions stay untouched; new ones are appended.
/// - A proposal with a blank label, a blank option label, fewer than two distinct options, or
///   a recommended option that is none of its options is dropped (with a line saying why).
///   Two proposals with the same artifact id in one pass: the first wins.
/// - Sizes are capped (label 200 characters, question and rationale 4 KB, at most 10 options of
///   200 characters each, descriptions 500 characters); anything cut gets a `trimmed` line.
/// - `DecisionRecord` has no field for the recommended option, so the rationale starts
///   `Recommended: {option}.`, `alternatives_considered` holds the other option labels and
///   `option_descriptions` the one-line descriptions (recommended first). The console reads
///   that back with `parse_recommended_rationale`.
pub(crate) fn merge_proposed_decisions(
    story_id: &str,
    existing: Vec<DecisionRecord>,
    proposals: &[ProposedDecisionRecord],
    now: chrono::DateTime<chrono::Utc>,
) -> DecisionMerge {
    let mut decisions = existing;
    let existing_count = decisions.len();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut proposed = 0usize;
    let mut replaced = 0usize;
    let mut ruled_already = 0usize;
    let mut dropped: Vec<String> = Vec::new();
    let mut trimmed: Vec<String> = Vec::new();

    for p in proposals {
        let mut cut: Vec<String> = Vec::new();
        let (label, label_cut) = cut_chars(p.label.trim(), MAX_LABEL_CHARS);
        if label.is_empty() {
            dropped.push("Dropped a proposed decision: it had no label.".to_string());
            continue;
        }
        if label_cut {
            cut.push("the label".to_string());
        }
        let name = label.as_str();

        // Options: every label must be present; repeats (ignoring case) count once.
        let mut options: Vec<(String, String)> = Vec::new();
        let mut blank_option = false;
        let (mut labels_cut, mut descriptions_cut) = (false, false);
        for o in &p.options {
            let (l, lc) = cut_chars(o.label.trim(), MAX_OPTION_CHARS);
            if l.is_empty() {
                blank_option = true;
                break;
            }
            let (d, dc) = cut_chars(o.description.trim(), MAX_OPTION_DESCRIPTION_CHARS);
            labels_cut |= lc;
            descriptions_cut |= dc;
            if !options.iter().any(|(e, _)| e.eq_ignore_ascii_case(&l)) {
                options.push((l, d));
            }
        }
        if blank_option || options.len() < 2 {
            dropped.push(format!(
                "Dropped the proposed decision \"{name}\": it needs at least two options, each with a label."
            ));
            continue;
        }
        if labels_cut {
            cut.push("option labels".to_string());
        }
        if descriptions_cut {
            cut.push("option descriptions".to_string());
        }

        let (recommended_given, _) = cut_chars(p.recommended.trim(), MAX_OPTION_CHARS);
        if recommended_given.is_empty() {
            dropped.push(format!(
                "Dropped the proposed decision \"{name}\": it named no recommended option."
            ));
            continue;
        }
        // The recommendation must be one of the options (ignoring case); the option's own
        // spelling is used. One that matches none cannot be shown or approved honestly.
        let Some(rec_at) = options
            .iter()
            .position(|(l, _)| l.eq_ignore_ascii_case(&recommended_given))
        else {
            dropped.push(format!(
                "Dropped the proposed decision \"{name}\": its recommended option \"{recommended_given}\" is not one of its options."
            ));
            continue;
        };
        let (recommended, rec_description) = options.remove(rec_at);
        let option_count = options.len() + 1;
        if option_count > MAX_OPTIONS {
            cut.push(format!("options (kept {MAX_OPTIONS} of {option_count})"));
            options.truncate(MAX_OPTIONS - 1);
        }
        let alternatives: Vec<String> = options.iter().map(|(l, _)| l.clone()).collect();
        let descriptions: Vec<String> = std::iter::once(rec_description)
            .chain(options.iter().map(|(_, d)| d.clone()))
            .collect();
        let descriptions = if descriptions.iter().all(|d| d.is_empty()) {
            Vec::new()
        } else {
            descriptions
        };

        let (question, question_cut) = cut_bytes(p.question.trim(), MAX_TEXT_BYTES);
        let (reasoning, reasoning_cut) = cut_bytes(p.rationale.trim(), MAX_TEXT_BYTES);
        if question_cut {
            cut.push("the question".to_string());
        }
        if reasoning_cut {
            cut.push("the rationale".to_string());
        }

        let artifact_id = format!("{story_id}/decision/{}", slugify_decision_label(&label));
        if !seen.insert(artifact_id.clone()) {
            continue;
        }
        if !cut.is_empty() {
            trimmed.push(format!(
                "Trimmed the proposed decision \"{name}\": cut down {}.",
                cut.join(", ")
            ));
        }
        let record = DecisionRecord::ai_proposed(
            story_id,
            artifact_id.clone(),
            label.as_str(),
            question,
            recommended_rationale(&recommended, &reasoning),
            alternatives,
            now,
        )
        .with_option_descriptions(descriptions);
        match decisions.iter().position(|d| d.artifact_id == artifact_id) {
            Some(i) => match decisions[i].outcome {
                DecisionOutcome::Pending => {
                    decisions[i] = record;
                    proposed += 1;
                    replaced += 1;
                }
                DecisionOutcome::Approved | DecisionOutcome::Rejected { .. } => {
                    ruled_already += 1;
                }
            },
            None => {
                decisions.push(record);
                proposed += 1;
            }
        }
    }

    DecisionMerge {
        decisions,
        proposed,
        kept: existing_count - replaced,
        ruled_already,
        dropped,
        trimmed,
    }
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
         {kernel}\n\n\
         {grounding_block}\
         Story title: {title}\n\
         Story description: {desc}\n\n\
         Read the relevant context and produce a concise investigation note in Markdown that:\n\
         1. Restates what the story asks for in your own words.\n\
         2. Lists the ambiguities and open questions.\n\
         3. Surface the concrete DECISIONS the architect must resolve before any code is \
            written. For EACH one, call the `propose_decision` tool with the question, two or \
            more options, your recommended option and the reasoning (cite the files you read). \
            Also summarise them in the note. A decision you do not propose through the tool \
            does not exist for the gate.\n\
         4. States explicitly what is OUT of scope.\n\n\
         GROUND EVERY CLAIM: every factual statement about the repo must come from a file you \
         actually read this session; cite the file path for each such claim. If you did not \
         verify something, say so rather than assert it.\n\n\
         If a SINGLE product/design decision genuinely BLOCKS your analysis and you cannot \
         make it yourself, call the `ask_clarification` tool with a structured question \
         (options each with a benefit/drawback) instead of guessing, then STOP (you will \
         be resumed with the human's answer).\n\n\
         Output ONLY the investigation note. The architect reviews it and rules on each \
         decision you proposed; no code is written until those decisions are approved.",
        kernel = camerata_app_core::GOVERNANCE_KERNEL_READONLY,
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
    repo_dir: Option<std::path::PathBuf>,
    // MULTI-REPO READ scope: the local clones of ALL the active project's repos. A project
    // has several repos; the investigation agent runs with `repo_dir` (the primary) as its
    // cwd but must be able to READ across all of them, so each is added via `--add-dir`.
    read_dirs: Vec<std::path::PathBuf>,
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
        // HONESTY GATE (NO placeholders): without a live agent there is no analysis to record, so
        // we REFUSE rather than write a synthetic "pending" note that masquerades as a real
        // investigation artifact. Fail the run loudly and leave the UoW investigation note
        // UNTOUCHED. Mirrors the token-free refusals in dev_implement_run / pr_resolve_run /
        // update_branch_run.
        runs.push_event(
            &run_id,
            GateEvent {
                seq: next_seq(),
                layer: "investigation".to_string(),
                verdict: "error".to_string(),
                rule: None,
                detail: "Investigation requires the AI agent, but live mode is off. \
                         Set CAMERATA_LIVE_BUILD=1 to run the real single-agent analysis."
                    .to_string(),
                content_hash: None,
            },
        );
        uow.append_history(
            &story_id,
            "note",
            "Investigation run refused: live mode is off (set CAMERATA_LIVE_BUILD=1). No analysis \
             agent ran and no investigation note was recorded.",
        );
        runs.fail_with_reason(
            &run_id,
            "Investigation requires the AI agent. Set CAMERATA_LIVE_BUILD=1 to run this analysis."
                .to_string(),
        );
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
        repo_dir,
        read_dirs,
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
// too_many_arguments: the resume entry point takes the app's stores by value (each is a cheap
// `Arc` handle) plus the persisted resume context; they are independent, not one concept.
#[allow(clippy::too_many_arguments)]
pub async fn resume_investigation_after_clarification(
    runs: RunStore,
    uow: UowStore,
    clarifications: ClarificationStore,
    resume: ClarifyResumeStore,
    ctx: ClarifyResumeContext,
    answer_summary: String,
    repo_dir: Option<std::path::PathBuf>,
    // MULTI-REPO READ scope: ALL the active project's local repo clones (read-only --add-dir).
    read_dirs: Vec<std::path::PathBuf>,
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

    let task =
        investigation_resume_prompt(&ctx.original_task, &ctx.asked_question, &answer_summary);
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
        repo_dir,
        read_dirs,
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
    repo_dir: Option<std::path::PathBuf>,
    read_dirs: Vec<std::path::PathBuf>,
    next_seq: impl Fn() -> usize,
) {
    let gateway_bin = match locate_gateway_bin() {
        Ok(bin) => bin,
        Err(e) => {
            // BUG B: a setup failure is a genuine Failed terminal (with the reason attached
            // to the run AND the UoW history), not a silent AwaitingQa with no note recorded.
            let reason = format!(
                "Investigation needs the gateway binary: {e}. Build it with \
                 `cargo build -p camerata-gateway`, then retry."
            );
            runs.push_event(
                &run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "setup".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: reason.clone(),
                    content_hash: None,
                },
            );
            uow.append_history(
                &story_id,
                "note",
                &format!("Investigation run failed: {reason}"),
            );
            runs.fail_with_reason(&run_id, reason);
            return;
        }
    };

    // Build the SAME governed role the fleet uses (every enforced gate rule in force),
    // and prepare ONE gated session. This is what makes the investigation agent carry
    // the identical universal tool gate: allowedTools = gated tools only, Task disallowed.
    let role = match governed_role("Investigator").await {
        Ok(r) => r,
        Err(e) => {
            let reason = format!("Could not build the governed investigator role: {e}");
            runs.push_event(
                &run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "setup".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: reason.clone(),
                    content_hash: None,
                },
            );
            uow.append_history(
                &story_id,
                "note",
                &format!("Investigation run failed: {reason}"),
            );
            runs.fail_with_reason(&run_id, reason);
            return;
        }
    };

    // ON-DEMAND REPO READ (the core invariant): bind the agent to the active project's
    // local clone. `prepare_session(..., Some(dir))` sets the driver's cwd + `--add-dir`
    // so the read-only built-ins (Read/Grep/Glob/LS) can open ANY file in the repo being
    // investigated — not just the digest in the prompt. It ALSO sets the gateway write-jail
    // to that dir; that is independent and does not loosen anything. The agent's only write
    // path is still `gated_write` (Task/Write/Bash disallowed by the driver), now confined
    // to the project repo. When the repo isn't local (`None`), the agent inherits the
    // orchestrator cwd and works from the digest only — degraded, but still gated.
    // The session temp dir is RAII-managed inside SessionSpawn._dir (ARCH-RESOURCE-LIFECYCLE-1);
    // a unique dir is created per prepare_session call so a resume's sink never collides.
    // MULTI-REPO READ: cwd/write-jail stay the primary repo (`repo_dir`); ALL the project's
    // repo clones are added as read-only `--add-dir` so the investigation agent can read
    // across every repo. The write jail is NOT widened (still `repo_dir`); the primary is
    // deduped against the cwd inside the driver.
    let spawn = match prepare_session(&gateway_bin, &role, repo_dir.as_deref(), &read_dirs, None) {
        Ok(s) => s,
        Err(e) => {
            let reason = format!("Could not prepare the investigation session: {e}");
            runs.push_event(
                &run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "setup".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: reason.clone(),
                    content_hash: None,
                },
            );
            uow.append_history(
                &story_id,
                "note",
                &format!("Investigation run failed: {reason}"),
            );
            runs.fail_with_reason(&run_id, reason);
            return;
        }
    };

    // Opt the investigation agent into the READ-CLASS ask_clarification tool (Phase 3b).
    // This adds NO write path: the gate (gated_write only) and the disallowed-builtins
    // denylist (Task/Write/Bash/…) are unchanged.
    // Wire the activity heartbeat so streamed agent output keeps last_activity_ms fresh.
    // Destructure spawn so the _dir (TempDir RAII guard) is held independently while
    // the driver is consumed. The dir stays alive until `_session_dir` is dropped.
    let camerata_agent::SessionSpawn {
        driver: raw_driver,
        _dir: _session_dir,
        ..
    } = spawn;
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

    // Both sinks are read inside `finish_investigation_pass`, before the session dir goes away.
    finish_investigation_pass(
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
        _session_dir.path(),
        agent_result,
        &next_seq,
    );
    // _session_dir (TempDir) removes the session dir automatically on drop here.
}

/// What happens once the agent has returned: read its two sinks, then either PAUSE on a raised
/// clarification or record the note. Extracted from [`run_one_investigation_pass`] so the
/// wiring is unit-testable without an agent; the pause and resume functions are unchanged.
///
/// Phase 3b: if the agent raised a clarifying question, PAUSE (post the question into the 3a
/// store, persist the resume context, park the run at `AwaitingClarification`). The agent
/// already STOPped (the question was its last act). Only reachable on `Ok` (an error has no
/// session output to read the question back out of). Decisions the agent proposed BEFORE
/// asking are filed first, so the pause does not throw them away.
#[allow(clippy::too_many_arguments)]
fn finish_investigation_pass(
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
    session_dir: &Path,
    agent_result: anyhow::Result<camerata_core::AgentOutcome>,
    next_seq: &impl Fn() -> usize,
) {
    // The decisions the agent filed through `propose_decision` this pass.
    let proposed = read_proposed_decisions(session_dir);
    if agent_result.is_ok() {
        if let Some(req) = read_first_clarify_request(session_dir) {
            if !proposed.is_empty() {
                record_proposed_decisions(runs, uow, run_id, story_id, &proposed, next_seq);
            }
            pause_run_on_clarification(
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
                req,
                next_seq(),
            );
            return;
        }
    }
    record_investigation_outcome(
        runs,
        uow,
        run_id,
        story_id,
        agent_result,
        proposed,
        next_seq,
    );
}

/// Record the investigation agent's raw result onto the run + UoW: either a real note +
/// AwaitingQa, or (BUG B) a genuine `Failed` terminal with a reason — never a silent
/// AwaitingQa with an empty/missing note. Extracted from [`run_one_investigation_pass`] so
/// this decision is unit-testable without spawning a real agent (the full pass needs a
/// working `camerata-gateway` binary + a real `claude` subprocess; this function needs
/// neither).
fn record_investigation_outcome(
    runs: &RunStore,
    uow: &UowStore,
    run_id: &str,
    story_id: &str,
    agent_result: anyhow::Result<camerata_core::AgentOutcome>,
    proposed: Vec<ProposedDecisionRecord>,
    next_seq: &impl Fn() -> usize,
) {
    match agent_result {
        Ok(outcome) => {
            // BUG B: an agent pass that produced no usable text is NOT a silent success —
            // recording an empty note and walking to AwaitingQa left the architect staring
            // at "no note recorded" with no idea whether the run ever actually happened.
            // Treat "no output" as a genuine failure (LIFECYCLE-2: a failure is not a
            // success) so the reason surfaces on the empty-state UI and the run is
            // re-runnable.
            if outcome.result.trim().is_empty() {
                let reason =
                    "The investigation agent completed but produced no analysis note (empty \
                     output)."
                        .to_string();
                runs.push_event(
                    run_id,
                    GateEvent {
                        seq: next_seq(),
                        layer: "investigation".to_string(),
                        verdict: "error".to_string(),
                        rule: None,
                        detail: format!("Investigation failed: {reason}"),
                        content_hash: None,
                    },
                );
                uow.append_history(
                    story_id,
                    "note",
                    &format!("Investigation run failed: {reason}"),
                );
                runs.fail_with_reason(run_id, reason);
                return;
            }

            // No question raised: record the analysis verbatim as the investigation note.
            // This is honest: the note IS the model's output, attributed to the AI,
            // awaiting the architect's review (note.reviewed = false). No synthetic content.
            let note = InvestigationArtifact::ai_authored(
                story_id,
                outcome.result.clone(),
                chrono::Utc::now(),
            );
            uow.set_investigation_note(&note);
            uow.append_history(
                story_id,
                "note",
                "Investigation agent produced an analysis note (awaiting architect review).",
            );
            runs.push_event(
                run_id,
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
            record_proposed_decisions(runs, uow, run_id, story_id, &proposed, next_seq);
            runs.set_status(run_id, RunStatus::AwaitingQa, true);
        }
        Err(e) => {
            // BUG B: an agent-run error must be a genuine `Failed` terminal (with the
            // reason attached) rather than a silent AwaitingQa — previously the error was
            // only visible as a gate EVENT (buried in a stream the architect may never open)
            // while the run itself looked like it completed normally with an empty note.
            let reason = format!("Investigation agent failed: {e}");
            runs.push_event(
                run_id,
                GateEvent {
                    seq: next_seq(),
                    layer: "investigation".to_string(),
                    verdict: "error".to_string(),
                    rule: None,
                    detail: reason.clone(),
                    content_hash: None,
                },
            );
            uow.append_history(
                story_id,
                "note",
                &format!("Investigation run failed: {reason}"),
            );
            runs.fail_with_reason(run_id, reason);
        }
    }
}

/// File the decisions the agent proposed this pass onto the story and say so in the run's
/// events and the story history. Never overwrites an `Approved` or `Rejected` record (see
/// [`merge_proposed_decisions`]). Extracted so it is unit-testable without an agent.
fn record_proposed_decisions(
    runs: &RunStore,
    uow: &UowStore,
    run_id: &str,
    story_id: &str,
    proposed: &[ProposedDecisionRecord],
    next_seq: &impl Fn() -> usize,
) {
    // KNOWN RACE (docs/BACKLOG.md): this reads the decision list, merges, then writes the whole
    // list back with no lock across the three steps. A decision the architect rules on in the
    // console between the read and the write can be overwritten by the stale list. The window
    // is one pass's end, short and rare; a compare-and-swap on the list is the fix.
    let existing = uow.decisions_for(story_id);
    let had_existing = !existing.is_empty();
    let merge = merge_proposed_decisions(story_id, existing, proposed, chrono::Utc::now());
    let info = |detail: String, seq: usize| GateEvent {
        seq,
        layer: "investigation".to_string(),
        verdict: "info".to_string(),
        rule: None,
        detail,
        content_hash: None,
    };

    for line in merge.dropped.iter().chain(merge.trimmed.iter()) {
        runs.push_event(run_id, info(line.clone(), next_seq()));
    }
    if !proposed.is_empty() {
        runs.push_event(
            run_id,
            info(
                format!(
                    "Investigation proposed {} decision(s) for review; {} kept from before.",
                    merge.proposed, merge.kept
                ),
                next_seq(),
            ),
        );
    }
    if merge.proposed > 0 {
        uow.set_decisions(story_id, merge.decisions);
        uow.append_history(
            story_id,
            "note",
            &format!(
                "Investigation proposed {} decision(s). Review and approve them before development.",
                merge.proposed
            ),
        );
    } else if proposed.is_empty() && !had_existing {
        uow.append_history(
            story_id,
            "note",
            "The investigation proposed no decisions through the tool. Add at least one by hand before development.",
        );
    } else if proposed.is_empty() {
        uow.append_history(
            story_id,
            "note",
            "The investigation proposed no new decisions through the tool.",
        );
    } else if merge.ruled_already > 0 {
        uow.append_history(
            story_id,
            "note",
            "The investigation proposed no new decisions; you had already ruled on the ones it raised.",
        );
    } else {
        uow.append_history(
            story_id,
            "note",
            "The investigation proposed no usable decisions. Add at least one by hand before development.",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use camerata_worktracker::investigation::{decisions_approved_for_development, RevisionActor};

    /// BUG B: an agent pass that comes back `Ok` but with EMPTY output must be a genuine
    /// `Failed` terminal (reason attached), never a silent `AwaitingQa` with an empty note.
    /// Exercises [`record_investigation_outcome`] directly (no real agent/gateway binary
    /// needed — that would spend real tokens and take real wall-clock time in an
    /// environment where `camerata-gateway` happens to already be built).
    #[test]
    fn record_investigation_outcome_fails_honestly_on_empty_output() {
        let runs = RunStore::new();
        let uow = UowStore::new();
        let run_id = runs.create("CAM-INV-EMPTY", "live", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

        let outcome = camerata_core::AgentOutcome {
            session_id: "sess-1".to_string(),
            result: "   \n\t  ".to_string(), // whitespace-only: not a usable note
            cost_usd: None,
            denials: Vec::new(),
        };
        record_investigation_outcome(
            &runs,
            &uow,
            &run_id,
            "CAM-INV-EMPTY",
            Ok(outcome),
            Vec::new(),
            &next_seq,
        );

        let run = runs.get(&run_id).expect("run exists");
        assert!(run.done, "a failed run is terminal");
        match &run.status {
            RunStatus::Failed { reason } => {
                assert!(
                    reason.to_lowercase().contains("no analysis note")
                        || reason.to_lowercase().contains("empty"),
                    "reason must explain there was no usable output: {reason}"
                );
                assert_eq!(run.failure_reason.as_deref(), Some(reason.as_str()));
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        // No "note recorded" allow-verdict event was pushed — only the failure event.
        assert!(
            !run.events.iter().any(|e| e.verdict == "allow"),
            "empty output must not be recorded as a successful note"
        );
        let history = uow.get_or_create("CAM-INV-EMPTY").history;
        assert!(
            history
                .iter()
                .any(|h| h.text.contains("Investigation run failed")),
            "the failure must be recorded on the UoW history, not just the run's events"
        );
    }

    /// BUG B: an agent-run ERROR (the driver itself failed) must ALSO land on a genuine
    /// `Failed` terminal with the underlying error in the reason — not a silent AwaitingQa.
    #[test]
    fn record_investigation_outcome_fails_honestly_on_agent_error() {
        let runs = RunStore::new();
        let uow = UowStore::new();
        let run_id = runs.create("CAM-INV-ERR", "live", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

        let err: anyhow::Result<camerata_core::AgentOutcome> =
            Err(anyhow::anyhow!("subprocess exited non-zero"));
        record_investigation_outcome(
            &runs,
            &uow,
            &run_id,
            "CAM-INV-ERR",
            err,
            Vec::new(),
            &next_seq,
        );

        let run = runs.get(&run_id).expect("run exists");
        assert!(run.done, "a failed run is terminal");
        match &run.status {
            RunStatus::Failed { reason } => {
                assert!(
                    reason.contains("subprocess exited non-zero"),
                    "reason must carry the underlying error: {reason}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
        assert!(
            !run.events.iter().any(|e| e.verdict == "allow"),
            "an agent error must not be recorded as a successful note"
        );
    }

    /// Regression: a real, non-empty note still records normally and completes to
    /// AwaitingQa (BUG B only changes the empty/error paths).
    #[test]
    fn record_investigation_outcome_records_a_real_note_and_completes() {
        let runs = RunStore::new();
        let uow = UowStore::new();
        let run_id = runs.create("CAM-INV-OK", "live", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

        let outcome = camerata_core::AgentOutcome {
            session_id: "sess-2".to_string(),
            result: "A real investigation note with actual findings.".to_string(),
            cost_usd: Some(0.01),
            denials: Vec::new(),
        };
        record_investigation_outcome(
            &runs,
            &uow,
            &run_id,
            "CAM-INV-OK",
            Ok(outcome),
            Vec::new(),
            &next_seq,
        );

        let run = runs.get(&run_id).expect("run exists");
        assert_eq!(run.status, RunStatus::AwaitingQa);
        assert!(run.done);
        // `UowStore::new()` has no attached artifact store (in-memory tests), so
        // `investigation_note_for` no-ops either way — assert on the recorded gate event
        // instead, matching `investigation_run_token_free_records_placeholder_note_and_completes`.
        assert!(
            run.events
                .iter()
                .any(|e| e.verdict == "allow" && e.detail.contains("Investigation note recorded")),
            "a real note must be recorded as a successful event"
        );
    }

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

    /// The investigation prompt embeds the read-only governance kernel and the
    /// cite-the-files-you-read grounding mandate.
    #[test]
    fn investigation_prompt_embeds_readonly_kernel_and_grounding_mandate() {
        let p = investigation_prompt("CAM-7", "Add export", "Members CSV export.", None);
        assert!(
            p.contains("=== CAMERATA OPERATING PROTOCOL (analysis) ==="),
            "investigation prompt must embed the read-only governance kernel"
        );
        assert!(
            p.contains("GROUND EVERY CLAIM"),
            "investigation prompt must mandate grounding every claim in a file it read"
        );
        assert!(
            p.contains("cite the file path"),
            "investigation prompt must require citing the file for each repo claim"
        );
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
        use camerata_agent::{
            prepare_session, ASK_CLARIFICATION_TOOL, GATED_WRITE_TOOL, PROPOSE_DECISION_TOOL,
        };
        use camerata_core::{Role, RuleId};

        let role = Role {
            name: "Investigator".to_string(),
            rule_subset: vec![RuleId("GOV-1".to_string())],
            allowed_paths: vec!["crates/".to_string()],
        };
        // prepare_session now creates its own RAII TempDir internally (ARCH-RESOURCE-LIFECYCLE-1).
        let spawn = prepare_session(Path::new("/bin/camerata-gateway"), &role, None, &[], None)
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
        // The Investigator role is also allowed the recording-only propose_decision tool.
        assert!(allowed.split(' ').any(|t| t == PROPOSE_DECISION_TOOL));
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
                "claude-opus-5",
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
        assert_eq!(
            restored_q.question,
            "Include archived members in the export?"
        );

        // The resume context survived and carries enough to re-spawn with the Q in context.
        let ctx = resume
            .get(&parked_clar_id)
            .expect("resume ctx survived reload");
        assert_eq!(ctx.run_id, run_id);
        assert_eq!(ctx.phase, PausedPhase::Investigation);
        assert_eq!(ctx.model, "claude-opus-5");
        assert!(ctx.original_task.contains("CAM-7"));
        assert_eq!(
            ctx.asked_question,
            "Include archived members in the export?"
        );

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
    #[serial_test::serial(process_env)]
    async fn investigation_run_honors_cancel_before_start() {
        // A cancel that landed before the executor ran leaves the run terminal Cancelled
        // and does NOT advance it to AwaitingQa or record a placeholder note.
        let _env = camerata_test_env::EnvGuard::remove("CAMERATA_LIVE_BUILD");
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
            "claude-opus-5".to_string(),
            None,
            None,
            Vec::new(),
        )
        .await;

        let run = runs.get(&run_id).expect("run exists");
        assert_eq!(run.status, RunStatus::Cancelled);
        assert!(run.done);
        // No placeholder investigation event was recorded (the executor returned early).
        assert!(!run
            .events
            .iter()
            .any(|e| e.detail.contains("live mode off")));
    }

    #[tokio::test]
    #[serial_test::serial(process_env)]
    async fn investigation_run_without_live_refuses_and_leaves_note_untouched() {
        // Live mode off (default in tests): no agent spawned. The run now REFUSES honestly
        // (Failed) rather than writing a synthetic "pending" note. The UoW investigation note
        // stays UNTOUCHED. This is the CI-safe path.
        let _env = camerata_test_env::EnvGuard::remove("CAMERATA_LIVE_BUILD");
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
            "claude-opus-5".to_string(),
            None,
            None,
            Vec::new(),
        )
        .await;

        let run = runs.get(&run_id).expect("run exists");
        // The run FAILS loudly — it does NOT fabricate a completed investigation.
        assert!(
            matches!(run.status, RunStatus::Failed { .. }),
            "a token-free investigation must fail, not complete with a placeholder"
        );
        assert!(run.done);
        // An honest error event is present and clearly labelled as live-mode-off.
        assert!(run
            .events
            .iter()
            .any(|e| e.verdict == "error" && e.detail.contains("live mode is off")));
        // The UoW investigation note was NOT written (no synthetic content).
        assert!(
            uow.investigation_note_for("CAM-INV-1").is_none(),
            "a refused investigation must leave the note untouched"
        );
        // The history records the refusal, not a fabricated "note recorded" entry.
        let uow_state = uow.get_or_create("CAM-INV-1");
        assert!(
            uow_state
                .history
                .iter()
                .any(|h| h.text.contains("Investigation run refused")),
            "the refusal must be recorded honestly in the UoW history"
        );
        assert!(
            !uow_state
                .history
                .iter()
                .any(|h| h.text.contains("placeholder note")),
            "no placeholder-note history line may be written"
        );
        // Token-free path posts no clarification.
        assert!(clarifications.for_story("CAM-INV-1").is_empty());
    }

    // ── Gap 9: the investigation files structured decisions ─────────────────

    fn session_dir_for(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cam-inv-decisions-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn proposal(label: &str, options: &[&str], recommended: &str) -> ProposedDecisionRecord {
        ProposedDecisionRecord {
            label: label.to_string(),
            question: format!("What about {label}?"),
            options: options
                .iter()
                .map(|o| ProposedOptionRecord {
                    label: (*o).to_string(),
                    description: String::new(),
                })
                .collect(),
            recommended: recommended.to_string(),
            rationale: "Reads src/lib.rs.".to_string(),
        }
    }

    fn at() -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap()
    }

    #[test]
    fn read_proposed_decisions_reads_every_valid_line_and_skips_malformed_ones() {
        let dir = session_dir_for("read");
        let good1 = serde_json::json!({
            "label": "Session storage",
            "question": "Where do sessions live?",
            "options": [
                {"label": "Cookies", "description": "server side"},
                {"label": "JWT", "description": "stateless"}
            ],
            "recommended": "Cookies",
            "rationale": "src/auth.rs sets a cookie.",
            "ts_ms": 1u64
        })
        .to_string();
        let good2 = serde_json::json!({
            "label": "Pagination",
            "question": "Cursor or offset?",
            "options": [{"label": "Cursor"}, {"label": "Offset"}],
            "recommended": "Cursor",
            "rationale": "",
            "ts_ms": 2u64
        })
        .to_string();
        let no_label = serde_json::json!({"question": "x", "recommended": "y"}).to_string();
        std::fs::write(
            dir.join("decisions.jsonl"),
            format!("{good1}\nnot json at all\n\n{no_label}\n{{\"label\": \n{good2}\n"),
        )
        .unwrap();

        let got = read_proposed_decisions(&dir);
        assert_eq!(got.len(), 2, "both valid lines, no malformed ones: {got:?}");
        assert_eq!(got[0].label, "Session storage");
        assert_eq!(got[0].options.len(), 2);
        assert_eq!(got[0].options[1].label, "JWT");
        assert_eq!(got[0].recommended, "Cookies");
        assert_eq!(got[1].label, "Pagination");
        assert_eq!(got[1].options[0].description, "");

        // No sink at all is simply "nothing proposed".
        let empty = session_dir_for("read-empty");
        assert!(read_proposed_decisions(&empty).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn merge_proposed_decisions_replaces_pending_keeps_approved_and_rejected_and_dedups_by_artifact_id(
    ) {
        let story = "acme/api#4";
        let old = at();
        let record = |slug: &str, label: &str| {
            DecisionRecord::ai_proposed(
                story,
                format!("{story}/decision/{slug}"),
                label,
                "old question",
                "Recommended: Old. old reasoning",
                vec!["Other".to_string()],
                old,
            )
        };
        let approved = record("auth", "Auth").approve(old);
        let rejected = record("cache", "Cache").reject("wrong question", old);
        let pending = record("paging", "Paging");
        let hand_typed = record("logging", "Logging").approve(old);
        let existing = vec![
            approved.clone(),
            rejected.clone(),
            pending.clone(),
            hand_typed.clone(),
        ];

        let proposals = vec![
            proposal("Auth", &["A", "B"], "A"),
            proposal("Cache", &["A", "B"], "B"),
            proposal("Paging", &["Cursor", "Offset"], "Cursor"),
            proposal("Paging", &["X", "Y"], "X"),
            proposal("Storage", &["Cookies", "JWT"], "JWT"),
        ];
        let now = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        let merge = merge_proposed_decisions(story, existing, &proposals, now);

        let ids: Vec<&str> = merge
            .decisions
            .iter()
            .map(|d| d.artifact_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec![
                "acme/api#4/decision/auth",
                "acme/api#4/decision/cache",
                "acme/api#4/decision/paging",
                "acme/api#4/decision/logging",
                "acme/api#4/decision/storage",
            ],
            "existing order is stable; replaced in place; new appended"
        );
        // Approved and rejected are untouched, byte for byte.
        assert_eq!(merge.decisions[0], approved);
        assert_eq!(merge.decisions[1], rejected);
        assert_eq!(merge.decisions[3], hand_typed);
        // The pending one was replaced by the fresh AI proposal (first of the duplicates).
        let paging = &merge.decisions[2];
        assert_eq!(paging.outcome, DecisionOutcome::Pending);
        assert_eq!(paging.provenance.actor, RevisionActor::Ai);
        assert_eq!(paging.provenance.at, now);
        assert_eq!(paging.question, "What about Paging?");
        assert_eq!(paging.rationale, "Recommended: Cursor. Reads src/lib.rs.");
        assert_eq!(paging.alternatives_considered, vec!["Offset".to_string()]);
        // A brand new one is Pending with AI provenance.
        let storage = &merge.decisions[4];
        assert_eq!(storage.label, "Storage");
        assert_eq!(storage.story_id, story);
        assert_eq!(storage.outcome, DecisionOutcome::Pending);
        assert_eq!(storage.alternatives_considered, vec!["Cookies".to_string()]);
        assert!(storage.rationale.starts_with("Recommended: JWT. "));
        // Counts: two filed (paging, storage); the three untouched existing records.
        assert_eq!(merge.proposed, 2);
        assert_eq!(merge.kept, 3);
        assert_eq!(merge.ruled_already, 2);
        assert!(merge.dropped.is_empty());
    }

    #[test]
    fn a_proposal_with_one_option_is_dropped() {
        let proposals = vec![
            proposal("Lonely", &["Only"], "Only"),
            proposal("   ", &["A", "B"], "A"),
            proposal("No pick", &["A", "B"], "  "),
            proposal("Blank option", &["A", "  "], "A"),
            proposal("Fine", &["A", "B"], "a"),
        ];
        let merge = merge_proposed_decisions("CAM-1", Vec::new(), &proposals, at());
        assert_eq!(merge.decisions.len(), 1);
        assert_eq!(merge.decisions[0].label, "Fine");
        // The recommendation uses the option's own spelling and the other option is the alternative.
        assert!(merge.decisions[0].rationale.starts_with("Recommended: A. "));
        assert_eq!(
            merge.decisions[0].alternatives_considered,
            vec!["B".to_string()]
        );
        assert_eq!(merge.proposed, 1);
        assert_eq!(merge.dropped.len(), 4, "each dropped proposal says why");
        assert!(merge.dropped[0].contains("Lonely"));
        assert!(merge.dropped[0].contains("two options"));
    }

    #[test]
    fn record_investigation_outcome_stores_proposed_decisions_as_pending_ai_records() {
        let runs = RunStore::new();
        let uow = UowStore::new();
        let story = "acme/api#9";
        let run_id = runs.create(story, "live", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;

        // The architect already ruled on one decision before this pass.
        let ruled = DecisionRecord::ai_proposed(
            story,
            format!("{story}/decision/auth"),
            "Auth",
            "q",
            "Recommended: A. r",
            vec!["B".to_string()],
            at(),
        )
        .approve(at());
        uow.set_decisions(story, vec![ruled.clone()]);

        let outcome = camerata_core::AgentOutcome {
            session_id: "sess".to_string(),
            result: "Investigation note.".to_string(),
            cost_usd: None,
            denials: Vec::new(),
        };
        let proposed = vec![
            proposal("Auth", &["A", "B"], "B"),
            proposal("Session storage", &["Cookies", "JWT"], "Cookies"),
            proposal("Pagination", &["Cursor", "Offset"], "Offset"),
            proposal("One option only", &["Solo"], "Solo"),
        ];
        record_investigation_outcome(
            &runs,
            &uow,
            &run_id,
            story,
            Ok(outcome),
            proposed,
            &next_seq,
        );

        let decisions = uow.decisions_for(story);
        assert_eq!(decisions.len(), 3, "the ruled one plus two new proposals");
        assert_eq!(
            decisions[0], ruled,
            "an approved record is never overwritten"
        );
        for d in &decisions[1..] {
            assert_eq!(d.outcome, DecisionOutcome::Pending);
            assert_eq!(d.provenance.actor, RevisionActor::Ai);
            assert!(d.rationale.starts_with("Recommended: "));
        }
        // Pending decisions keep the development gate shut.
        assert!(!decisions_approved_for_development(&decisions));

        let run = runs.get(&run_id).expect("run exists");
        assert_eq!(run.status, RunStatus::AwaitingQa);
        let details: Vec<&str> = run.events.iter().map(|e| e.detail.as_str()).collect();
        assert!(
            details
                .contains(&"Investigation proposed 2 decision(s) for review; 1 kept from before."),
            "events: {details:?}"
        );
        assert!(
            details.iter().any(|d| d.contains("One option only")),
            "the dropped proposal is named in an event: {details:?}"
        );
        let history: Vec<String> = uow
            .get_or_create(story)
            .history
            .into_iter()
            .map(|h| h.text)
            .collect();
        assert!(
            history.contains(
                &"Investigation proposed 2 decision(s). Review and approve them before development."
                    .to_string()
            ),
            "history: {history:?}"
        );
    }

    #[test]
    fn record_investigation_outcome_says_so_when_nothing_was_proposed() {
        let runs = RunStore::new();
        let uow = UowStore::new();
        let story = "acme/api#10";
        let run_id = runs.create(story, "live", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let outcome = camerata_core::AgentOutcome {
            session_id: "sess".to_string(),
            result: "A note that lists decisions only in prose.".to_string(),
            cost_usd: None,
            denials: Vec::new(),
        };
        record_investigation_outcome(
            &runs,
            &uow,
            &run_id,
            story,
            Ok(outcome),
            Vec::new(),
            &next_seq,
        );
        assert!(uow.decisions_for(story).is_empty());
        let history: Vec<String> = uow
            .get_or_create(story)
            .history
            .into_iter()
            .map(|h| h.text)
            .collect();
        assert!(
            history.contains(
                &"The investigation proposed no decisions through the tool. Add at least one by hand before development."
                    .to_string()
            ),
            "history: {history:?}"
        );
        // The run itself still completes: the note is real.
        assert_eq!(runs.get(&run_id).unwrap().status, RunStatus::AwaitingQa);
    }

    #[test]
    fn investigation_prompt_tells_the_agent_to_call_propose_decision_for_each_decision() {
        let p = investigation_prompt("CAM-7", "Add export", "Members CSV export.", None);
        assert!(p.contains("propose_decision"));
        assert!(p.contains("For EACH one, call the `propose_decision` tool"));
        assert!(p.contains("two or more options"));
        assert!(p.contains("your recommended option"));
        assert!(p.contains("cite the files you read"));
        assert!(p.contains(
            "A decision you do not propose through the tool does not exist for the gate."
        ));
        // ask_clarification stays for the one blocking question.
        assert!(p.contains("ask_clarification"));
        // The new wording follows the house rules: no em dashes in what was added.
        let section3 = p
            .split("3. ")
            .nth(1)
            .and_then(|r| r.split("4. ").next())
            .unwrap();
        assert!(!section3.contains('\u{2014}'));
    }

    // ── Gap 9 review fixes ──────────────────────────────────────────────────

    #[test]
    fn a_proposal_whose_recommended_option_matches_none_of_its_options_is_dropped_by_name() {
        let proposals = vec![
            proposal("Wrong pick", &["Cookies", "JWT"], "Carrier pigeon"),
            proposal("Right pick", &["Cookies", "JWT"], "jwt"),
        ];
        let merge = merge_proposed_decisions("CAM-1", Vec::new(), &proposals, at());
        assert_eq!(merge.decisions.len(), 1);
        assert_eq!(merge.decisions[0].label, "Right pick");
        assert_eq!(merge.dropped.len(), 1);
        assert!(
            merge.dropped[0].contains("Wrong pick"),
            "{:?}",
            merge.dropped
        );
        assert!(
            merge.dropped[0].contains("Carrier pigeon"),
            "names the bad recommendation: {:?}",
            merge.dropped
        );
    }

    #[test]
    fn oversized_proposals_are_trimmed_with_an_event_line() {
        let long_label = "L".repeat(300);
        let mut big = proposal(&long_label, &[], "Opt11");
        big.question = "q".repeat(5000);
        big.rationale = "é".repeat(3000); // 6000 bytes, two bytes per character
        big.options = (0..12)
            .map(|i| ProposedOptionRecord {
                label: format!("Opt{i}"),
                description: "d".repeat(600),
            })
            .collect();
        let merge = merge_proposed_decisions("CAM-1", Vec::new(), &[big], at());
        assert_eq!(merge.decisions.len(), 1, "trimmed, not dropped");
        let d = &merge.decisions[0];
        assert_eq!(d.label.chars().count(), 200);
        assert!(d.question.len() <= 4096);
        let (rec, reasoning) =
            camerata_worktracker::investigation::parse_recommended_rationale(&d.rationale).unwrap();
        assert_eq!(rec, "Opt11", "the recommendation survives the option cap");
        assert!(reasoning.len() <= 4096 && reasoning.chars().all(|c| c == 'é'));
        assert_eq!(d.alternatives_considered.len(), 9, "10 options in all");
        assert_eq!(d.alternatives_considered[0], "Opt0");
        assert_eq!(d.option_descriptions.len(), 10);
        assert!(d
            .option_descriptions
            .iter()
            .all(|x| x.chars().count() == 500));
        assert_eq!(merge.trimmed.len(), 1);
        let line = &merge.trimmed[0];
        for part in [
            "the label",
            "the question",
            "the rationale",
            "option descriptions",
            "kept 10 of 12",
        ] {
            assert!(line.contains(part), "{part} missing from: {line}");
        }
        // Small proposals produce no trim line.
        let ok = merge_proposed_decisions(
            "CAM-1",
            Vec::new(),
            &[proposal("Fine", &["A", "B"], "A")],
            at(),
        );
        assert!(ok.trimmed.is_empty());
    }

    #[test]
    fn option_descriptions_are_kept_recommended_first() {
        let mut p = proposal("Storage", &[], "JWT");
        p.options = vec![
            ProposedOptionRecord {
                label: "Cookies".to_string(),
                description: "server side".to_string(),
            },
            ProposedOptionRecord {
                label: "JWT".to_string(),
                description: "stateless".to_string(),
            },
            ProposedOptionRecord {
                label: "Sessions".to_string(),
                description: String::new(),
            },
        ];
        let merge = merge_proposed_decisions("CAM-1", Vec::new(), &[p], at());
        let d = &merge.decisions[0];
        assert_eq!(d.alternatives_considered, vec!["Cookies", "Sessions"]);
        assert_eq!(d.option_descriptions, vec!["stateless", "server side", ""]);
    }

    #[test]
    fn proposals_filed_before_a_clarification_pause_are_recorded() {
        let dir = session_dir_for("pause");
        let decision = serde_json::json!({
            "label": "Session storage",
            "question": "Where do sessions live?",
            "options": [{"label": "Cookies"}, {"label": "JWT"}],
            "recommended": "Cookies",
            "rationale": "src/auth.rs sets a cookie.",
            "ts_ms": 1u64
        })
        .to_string();
        std::fs::write(dir.join("decisions.jsonl"), format!("{decision}\n")).unwrap();
        let clarify = serde_json::json!({
            "question": "Include archived members?",
            "options": [{"label": "Yes"}, {"label": "No"}],
            "multi_select": false,
            "allow_free_text": true,
            "ts_ms": 2u64
        })
        .to_string();
        std::fs::write(dir.join("clarify-requests.jsonl"), format!("{clarify}\n")).unwrap();

        let runs = RunStore::new();
        let uow = UowStore::new();
        let clarifications = ClarificationStore::new();
        let resume = ClarifyResumeStore::new();
        let story = "acme/api#12";
        let run_id = runs.create(story, "investigation", crate::run::RunKind::Watched);
        let seq = std::sync::atomic::AtomicUsize::new(0);
        let next_seq = || seq.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        let outcome = camerata_core::AgentOutcome {
            session_id: "sess".to_string(),
            result: "partial note".to_string(),
            cost_usd: None,
            denials: Vec::new(),
        };
        finish_investigation_pass(
            &runs,
            &uow,
            &clarifications,
            &resume,
            &run_id,
            story,
            "Add export",
            "Members CSV export.",
            "claude-opus-5",
            "the task",
            &dir,
            Ok(outcome),
            &next_seq,
        );

        // The run paused on the question...
        assert_eq!(
            runs.get(&run_id).unwrap().status,
            RunStatus::AwaitingClarification
        );
        assert_eq!(clarifications.for_story(story).len(), 1);
        // ...and the decision filed before it was kept, still Pending.
        let decisions = uow.decisions_for(story);
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].label, "Session storage");
        assert_eq!(decisions[0].outcome, DecisionOutcome::Pending);
        assert_eq!(decisions[0].provenance.actor, RevisionActor::Ai);

        // A pause with nothing proposed files nothing and writes no "add one by hand" note.
        let dir2 = session_dir_for("pause-empty");
        std::fs::write(dir2.join("clarify-requests.jsonl"), format!("{clarify}\n")).unwrap();
        let story2 = "acme/api#13";
        let run2 = runs.create(story2, "investigation", crate::run::RunKind::Watched);
        finish_investigation_pass(
            &runs,
            &uow,
            &clarifications,
            &resume,
            &run2,
            story2,
            "t",
            "d",
            "m",
            "task",
            &dir2,
            Ok(camerata_core::AgentOutcome {
                session_id: "s".to_string(),
                result: "n".to_string(),
                cost_usd: None,
                denials: Vec::new(),
            }),
            &next_seq,
        );
        assert!(uow.decisions_for(story2).is_empty());
        assert!(!uow
            .get_or_create(story2)
            .history
            .iter()
            .any(|h| h.text.contains("by hand")));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }
}
