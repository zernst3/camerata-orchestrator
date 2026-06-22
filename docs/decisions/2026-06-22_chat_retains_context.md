# 2026-06-22 Chat retains conversation context

## The bug

The grounded research chatbot was stateless. `POST /api/chat` accepted
`{ prompt, model, system }` and immediately called
`LlmRequest::new(req.prompt).with_model(...)` — a single bare prompt with no
memory of prior turns. The model had no mechanism to recall earlier exchanges
and would say things like "this is the start of our conversation" mid-thread.

## Root cause

`LlmRequest` is a single-prompt type (matching the CLI path: one
`--system-prompt` + one user message). There is no native messages-array
concept at this layer, so conversation history cannot be threaded as a
structured list of turns. The CLI path is correct as-is; only the chat handler
needed to carry history.

## Fix: embed history in the prompt text

The simplest correct approach given the single-prompt constraint: the UI sends
prior turns as a `history: Vec<ChatTurn>` array alongside the new `prompt`, and
the server renders them into a transcript block prepended to the user's message:

```
Conversation so far:
User: <prior user turn>
Assistant: <prior assistant turn>
...

User's new message:
<new prompt>
```

The grounding system prompt (`system`, the four context layers) is passed
through unchanged. The LLM receives the full conversation transcript as
its user message while the grounding remains in the system slot.

### Key design decisions

- **Client carries history per request.** The UI accumulates turns in a
  `use_signal(Vec<Turn>)` and sends all prior turns with each POST. There is no
  server-side session state — each request is still stateless on the server. A
  page reload clears the history, which is the expected UX (the "New chat"
  button already clears the client signal).

- **Back-compat: empty history = previous behavior.** `history` is
  `#[serde(default)]` on the server, so old clients or callers that omit it
  get the exact single-prompt path. The `render_chat_prompt` helper returns the
  bare prompt unchanged when history is empty.

- **Token cap.** History is capped at the most-recent `CHAT_HISTORY_TURN_CAP`
  (20) messages before rendering. Oldest turns are dropped first (FIFO). At
  roughly 150-300 tokens per turn this keeps the history contribution well under
  10k tokens at the limit. The cap constant is named and exported for tests.

- **No server-side persistence across reloads.** This fix gives the model
  memory within a browser session (or desktop session). Persisting chat history
  across reloads/restarts is an optional future follow-up, tracked separately if
  desired.

## What changed

- `crates/server/src/lib.rs`: Added `ChatTurn { role, content }`, added
  `history: Vec<ChatTurn>` to `ChatReq`, added `CHAT_HISTORY_TURN_CAP = 20`,
  added `render_chat_prompt(history, prompt) -> String` helper, updated `chat`
  handler to call it.

- `crates/ui/src/chat.rs`: Added `ChatHistoryTurn`, added `turns_to_history`
  converter, updated `send_chat` signature to accept `Vec<ChatHistoryTurn>`,
  updated both send paths (Enter key + Send button) to snapshot prior turns
  before pushing the new user message and pass them to `send_chat`.

## Tests added (camerata-server)

Four groups covering the requirements from the fix specification:

- (a) History-present: prior user turn in output, prior assistant turn in output,
  new message appears after the transcript block, full exchange + new message.
- (b) Back-compat: empty history returns bare prompt unchanged (two variants).
- (c) Token-cap: turns beyond the cap are dropped (oldest first); turns at
  exactly the cap limit are all kept.
- (d) Role labels: "user" -> "User", "assistant" -> "Assistant", unknown roles
  -> "Assistant".

All tests are unit tests on `render_chat_prompt` directly (no model calls).
