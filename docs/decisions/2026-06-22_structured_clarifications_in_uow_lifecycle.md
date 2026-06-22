# Structured clarifications across the UoW lifecycle (auto-saved + resumable)

**Date:** 2026-06-22 · **Decided by:** Zach. Every back-and-forth/clarification point in the UoW
lifecycle (investigation, dev, future phases, story-authoring) should present a **structured
question** — like the `AskUserQuestion` UX: multiple options each with benefits/drawbacks, an
"Other" free-text escape, optional multi-select — NOT free-text ping-pong. And everything
auto-saved so the user can leave and resume at any pause point.

## What exists today (extend, don't rebuild)
- `crates/server/src/clarify.rs`: `Clarification { question: String, answer: Option<String>,
  addressee, state: Asked|Answered }` + `ClarificationStore` (in-memory) + post/answer endpoints +
  the cockpit "NEEDS YOU" queue. **Free-text only, in-memory only, posted manually** (not emitted
  by agents).
- `uow.rs` `AuthoringState.chat: Vec<AuthorChatMessage>` — the story-authoring free-text chat.
- `worktracker::investigation::DecisionRecord/DecisionOutcome` — structured investigation decisions.

## Decision — four upgrades (Phase 3; after worktrees + PR)

1. **Structured question model.** Extend `Clarification`: `options: Vec<ClarifyOption{label,
   description}>`, `multi_select: bool`, `allow_free_text: bool` (default true = the "Other" escape).
   The answer captures selected option label(s) + optional free-text. A pure free-text question =
   empty options + `allow_free_text`. Mirrors `AskUserQuestion` exactly. Keep `question`/`answer`
   for back-compat / the free-text leg.
2. **Auto-save + resume.** Give `ClarificationStore` a disk path + flush-on-mutate (like
   `InMemoryStoryStore::at` / uow.json). Pause points survive restart. The lifecycle PAUSES at an
   open clarification and RESUMES when answered. The open-clarification queue shows where each UoW
   is waiting.
3. **Agents emit structured questions.** At each clarification point — story-authoring chat
   (upgrade free-text → structured), investigation (decision questions), dev (product calls) — the
   LLM/agent GENERATES a structured `Clarification` via a question-authoring schema (structured
   output) instead of free text. Structured by default; free-text always supported.
4. **One reusable UI component.** An `AskUserQuestion`-style panel (options + benefits/drawbacks +
   "Other" free-text + multi-select) reused at every clarification point in the dev console.

## The hard part — pause/resume channel
- **LLM chat loops (story-authoring):** easy — the LLM emits a structured question (schema), the
  loop persists + pauses + resumes with the answer appended.
- **Gated dev agent mid-write:** harder — needs a channel for the agent to RAISE a structured
  question, the run to PAUSE (persist) and SURFACE it, and RESUME (feed the answer back / re-spawn
  with it in context) on answer. Design when building Phase 3. The gate is unchanged — asking a
  question is not a write.

## Status — split into 3a (done) and 3b (pending)

**Phase 3a — DONE (foundation + the easy clarification point).** Shipped on
`feat/structured-clarify-3a`:
- **Structured model** (`clarify.rs`): `ClarifyOption{label, description}`; `Clarification`
  gained `options`, `multi_select`, `allow_free_text` (all `#[serde(default)]`, `allow_free_text`
  defaults true); `ClarifyAnswer{selected, free_text}` stored on `answer_selection` with `answer`
  kept as the human-readable summary (selected labels + free-text). `post_structured` /
  `answer_structured` with the old `post` / `answer` as free-text shims. Back-compat preserved.
- **Auto-save + resume** (`clarify.rs` + `lib.rs`): `ClarificationStore::at(path)` (load-or-new) +
  flush-on-mutate; wired to `clarifications.json` in the data-dir block. Open questions + answers
  survive a restart = resume at any open question.
- **Endpoints** (`lib.rs`): `PostClarifyReq` / `AnswerReq` gained optional structured fields
  (serde default → free-text when absent); handlers call the structured store methods.
- **Story-authoring upgrade** (`uow_author`): the LLM now returns an optional `options` array; when
  present the question is emitted as a structured clarification (free-text fallback when absent).
- **Reusable UI** (`cockpit.rs`): an `AskUserQuestion`-style `ClarifyQuestion` component
  (options + benefit/drawback + radio/checkbox + "Other" free-text), reused in the
  story-authoring pause point AND a `NeedsYouQueue` (open clarifications across all stories).
- Tests: structured round-trip, multi-select, free-text back-compat, the persistence/resume
  guarantee, serde-default loading legacy JSON, summary string.

**Phase 3b — INVESTIGATION DONE; dev mid-write DEFERRED.** Shipped on
`feat/clarify-3b-gated`. The agent→run channel + pause/resume mechanism, wired into the
INVESTIGATION phase. The 3a structured model + store + UI are reused AS-IS; 3b adds only the
channel. **The gate is unchanged** — asking a question is a READ-class action, not a write.

What landed:
- **`ask_clarification` MCP tool** (`crates/gateway/src/main.rs`): alongside `gated_write`, a
  READ-CLASS tool. The agent calls it with a structured question
  `{question, options:[{label,description}], multi_select, allow_free_text}`. The gateway RECORDS
  it (like it records gate decisions) to a per-session `clarify-requests.jsonl` sink (a sibling of
  the rules file, OUTSIDE the worktree jail) and returns a "STOP and end your turn" instruction.
  It writes NO repo file, spawns nothing, escalates nothing → **no new write path**.
- **Driver opt-in** (`crates/agent/src/lib.rs`): `ASK_CLARIFICATION_TOOL` +
  `ClaudeCliDriver::with_clarification(true)` appends the tool to `--allowedTools`. The
  disallowed-builtins denylist (`Task`/`Write`/`Bash`/…) and the gated-write-only write path are
  **byte-for-byte unchanged**; tests assert this.
- **Pause = checkpoint + auto-save** (`investigation_run.rs`): after the investigation agent
  returns, the server reads the sink (`read_first_clarify_request`). If a question was raised it
  posts it into the 3a `ClarificationStore` (auto-saved), persists a `ClarifyResumeContext`
  (`clarify_resume.rs`, disk-backed flush-on-mutate like the 3a store), records a `clarification/
  pause` run event, and parks the run at the new `RunStatus::AwaitingClarification` (NOT done).
  No blocking long-poll: the subprocess already exited at the question (its last act).
- **Resume = re-spawn** (`investigation_run.rs` + the answer endpoint in `lib.rs`): answering the
  3a clarification consumes the resume context (once — no double-resume) and re-spawns the SAME
  gated agent (same `governed_role` + `prepare_session`, gate intact) with the original task + the
  asked question + the answer appended (`investigation_resume_prompt`).
- **Surfacing** (`cockpit.rs`): the parked run shows a "WAITING ON YOU" badge + a
  `clarification`-layer activity event in `LiveRunPanel`, plus an inline `RunClarificationPrompt`
  (reusing the 3a `ClarifyQuestion`) so the question is answered in place; the cross-story
  `NeedsYouQueue` already lists it.
- **Tests** (token-free): the gateway tool records a structured question to the sink and writes
  nothing else; the run pauses → persists → survives a reload → resumes (resume prompt carries the
  Q+A); the gate posture is unchanged with clarification on (allow/disallow lists asserted via the
  `prepare_session` driver, the same pattern as the fleet/update-branch runs).

**Deferred — dev-phase mid-write resume.** The `ask_clarification` tool and the
`with_clarification` opt-in are phase-agnostic (any gated agent CAN raise a question), but wiring
the pause/resume into `live_fleet::execute_live_run{,_tiered}` is deferred. Why: the dev fleet
spawns one agent PER plan task and orchestrates a stage sequence with a partial-write worktree and
a layer-2 bounce loop. Pausing one stage means parking the whole fleet pipeline mid-sequence and
resuming THAT stage's agent (the `--resume <session_id>` primitive exists on the driver, but the
fleet doesn't yet capture/persist per-stage session ids or the coordinator's position). That is a
separate, substantial piece of fleet-orchestration plumbing; it does NOT require any gate change.
Investigation was wired first because it is single-agent, read-oriented, and decision-shaped — the
natural fit per the spec.

## Sequence
worktrees (Phase 1) → PR lifecycle (Phase 2) → structured clarifications (Phase 3). Sequential
because all three touch `lib.rs` / `uow.rs` / `cockpit.rs`; concurrent agents there would collide.

Relates to [[2026-06-22_uow_ai_story_authoring]] (the chat this generalizes) and the UoW dev-run
architecture.
