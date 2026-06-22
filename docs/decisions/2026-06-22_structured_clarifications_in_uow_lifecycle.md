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

**Phase 3b — PENDING (the hard part).** Gated dev/investigation agent-emit + the pause/resume
channel: a mid-write dev/investigation agent RAISING a structured question, the run PAUSING
(persist) and SURFACING it, then RESUMING (feed the answer back / re-spawn with it in context) on
answer. The structured model + store + UI from 3a are reused as-is; 3b only adds the agent→run
channel. The gate is unchanged — asking a question is not a write.

## Sequence
worktrees (Phase 1) → PR lifecycle (Phase 2) → structured clarifications (Phase 3). Sequential
because all three touch `lib.rs` / `uow.rs` / `cockpit.rs`; concurrent agents there would collide.

Relates to [[2026-06-22_uow_ai_story_authoring]] (the chat this generalizes) and the UoW dev-run
architecture.
