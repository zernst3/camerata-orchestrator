# ADR: Governed-Development phase chats are live, project-grounded LLM conversations

**Date:** 2026-07-05
**Status:** Accepted (GAP-4 shipped on `fix/gap4-chat`)
**Related:** `2026-07-02_bombe-loading-invariant`, GAP-4 in `docs/plans/2026-07-05_escalation-decisions.md`

## Context

The three-phase Governed-Development cockpit (`crates/ui/src/cockpit/uow.rs`) hosts two
per-phase agent chats:

- **Investigation & Refinement** chat ("Agent chat (investigation scope)").
- **Development** chat ("Chat back (development scope)", shown during a clarification pause).

Both were STUBS: sending a message persisted the user turn to the UoW transcript, then
appended a hardcoded reply (`"(Investigation agent response — coming soon. TODO #105)"` /
`"(Development agent response — coming soon. TODO #105)"`). The transcript round-tripped,
but the architect never got a real, project-grounded answer.

Camerata already has the LLM chat seam it needs: the global in-app assistant posts to
`POST /api/chat` (`crates/server/src/lib.rs::chat`) with `{ prompt, model, system, history }`
and receives an `LlmResponse` back. The grounding lives entirely in the `system` prompt the
caller assembles; the endpoint is generic.

## Decision

**Wire both phase chats to the existing `POST /api/chat` endpoint** (the same one the
global assistant uses) rather than building a new endpoint. The phase-specific grounding is
carried in the `system` prompt; the prior transcript is carried in `history`.

Grounding per phase:

- **Investigation chat** is grounded in the story id, the **investigation note** (the agent's
  written findings), and the **approved decisions** for the story. So the architect can ask
  "why was this decided?", "what does this note mean?", "what is still open?" and get an
  answer about THIS refinement, not Camerata in general.
- **Development chat** is grounded in the story id and the **approved decisions** the
  development run is bound to build under. So questions about the run are answered against the
  decisions it must honor.

The grounding is fetched fresh (`fetch_investigation_review`) inside each send so the chat
reflects the latest note/decision state, not a session-start snapshot.

### Invariants honored

- **Bombe loading invariant:** each send holds a `crate::loading::LoadingGuard` for the whole
  async span (a real LLM call is in flight). See `2026-07-02_bombe-loading-invariant`.
- **Persistence:** the REAL AI turn is persisted to the phase transcript via the existing
  `append_investigation_chat` / `append_development_chat` (role `"agent"`), so the conversation
  round-trips on reload exactly as the stub did.
- **No dead-end affordances:** a failed send surfaces an **error toast** with the reason (the
  backend error body is preserved verbatim, mirroring the global assistant's `send_chat`), the
  optimistic user turn is rolled back out of the local view, and the input is cleared **only on
  success** so the draft survives a retry.

## Alternatives considered

- **A new dedicated phase-chat endpoint.** Rejected: `/api/chat` already accepts an arbitrary
  `system` + `history`, so a phase-grounded conversation needs no server change. Adding an
  endpoint would duplicate the completer/model-resolution plumbing.
- **Reusing the escalation-chat seam** (`chat_system_prompt` in `crates/app-core/src/escalation.rs`).
  Rejected as the primary path: that seam is grounded in an `Escalation` (a blocked routine/run
  awaiting a human decision), not in a story's investigation note + decisions. The phase chats
  are refinement/clarification conversations, not escalation reviews. The `/api/chat` seam is the
  right altitude; the escalation seam stays for its own surface.

## Consequences

- The phase-chat grounding builders (`investigation_chat_system_prompt`,
  `development_chat_system_prompt`), the request-body builder (`phase_chat_body`), the history
  converter (`phase_history_from_messages`), the approved-decision extractor
  (`approved_decision_lines`), and the send helper (`send_phase_chat`) are pure/testable seams
  in `uow.rs`; the async fetch cannot run under SSR, so the request-building and persistence
  logic is unit-tested directly.
- Model selection reuses the phase's existing model signals (investigation uses `invest_model`;
  development uses the Balanced tier), so no new model UI is introduced.
