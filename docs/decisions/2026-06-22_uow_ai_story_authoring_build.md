# Build: author a story from a blank UoW with AI (push to board + auto-link)

**Date:** 2026-06-22 · Implements
[[2026-06-22_uow_ai_story_authoring]] (the design). Confined to `crates/server/**`
(`uow.rs`, `lib.rs`, reusing `onboard::create_issue` + `llm.rs` + `github_issues.rs`)
and `crates/ui/src/cockpit.rs` (+ `style.rs`).

## The three endpoints

1. **`POST /api/uow/blank`** → `{ uow_id }`. Creates a blank DRAFT UoW: a `draft-<token>`
   id, `work_item = None`, and an empty `authoring` state. It lists in `/api/uows` with
   `work_item: null` and `authoring: true`.

2. **`POST /api/uow/:story_id/author`** body `{ message }` → the updated `UnitOfWork`.
   The first message is the requirements prompt; subsequent ones are chat turns. The
   handler appends the user message to the UoW's chat, calls `Llm::complete` with a
   story-authoring system prompt that returns a minified JSON object
   `{ "title", "body", "reply" }`, updates `draft_title` / `draft_body`, appends the AI
   reply, persists, and returns the UoW. The system prompt instructs the model to ASK ONE
   clarifying question when the requirements are ambiguous. `enc_seg`-safe (the UI encodes
   the id; axum decodes it). **Token-less / LLM-off degrades gracefully**: the user turn is
   still saved, the draft is left unchanged, and the AI turn carries a clear "AI drafting
   is unavailable" note instead of crashing.

3. **`POST /api/uow/:story_id/publish`** body `{ repo: "owner/repo" }` →
   `{ uow_id, work_item }`. Reuses `onboard::create_issue(owner, repo, token, draft_title,
   draft_body)` to open the GitHub issue, parses the new issue number from the returned
   `html_url`, builds the canonical story via `github_issues::issue_to_story` and upserts it
   onto the spine (like `uow_from_workitem`), then **links** the draft UoW to it. Requires a
   GitHub token; returns a non-2xx with a clear reason when the token is absent, the repo is
   malformed, or the draft has no title. Returns the linked work item.

## UnitOfWork additions (`uow.rs`)

- `authoring: Option<AuthoringState>` — `Some` for a draft UoW being authored, `None`
  otherwise. `AuthoringState = { requirements_prompt, chat: Vec<AuthorChatMessage{role,
  text}>, draft_title, draft_body }`.
- `work_item: Option<String>` — the linked work-item story id for a UoW whose KEY is not
  itself the work-item story id (i.e. a published draft). `None` for a normal UoW (its key
  IS the work-item story id) and for an unpublished draft.

Both fields are `#[serde(default)]`, so a legacy `uow.json` (written before these existed)
deserializes unchanged. New store methods: `create_blank`, `append_authoring_turn`,
`link_work_item` (all persist via the existing `uow.json` flush).

## Draft-id-no-rekey choice

The draft UoW keeps its `draft-<token>` id as its store key for its whole lifecycle. On
publish we do NOT re-key it to `owner/repo#num`; instead the new `work_item` field carries
the real work-item story id. This avoids a re-key migration (and any in-flight run/lifecycle
state keyed by the draft id stays valid). `/api/uows` resolves a draft's work item by its
`work_item` link, falling back to the key for a normal UoW.

## UI authoring panel (`cockpit.rs`)

- **`NewAuthoredUowButton`** in the Governed Development left nav — calls `/api/uow/blank`
  and selects the new draft so the authoring panel opens.
- **`StoryAuthoringPanel`** renders for a draft (authoring) UoW instead of `UowDevControls`:
  a clarification chat (type → `POST /author` → show the AI reply + refreshed draft), a live
  draft preview (title + body via `crate::md::md_to_html`), a target-repo picker (the active
  project's repos), and a "Push to board & link" button (`POST /publish`). On success the
  UoW becomes a normal linked UoW and `UowDevControls` takes over (re-select the same id).
- `UowListEntry.work_item` is now `Option<WorkItem>` and gains an `authoring: bool`; the UoW
  cards show "Untitled draft story" / "Authoring" for an unpublished draft. Styles added to
  `style.rs` (`authoring-chat`, `authoring-msg`, `authoring-preview`, `authoring-publish`).

## Gate note

Story authoring is **LLM text generation** (drafts/refines an issue) — NOT a code-writing
agent. There is **no `gated_write` and no code writes** in this path, so the development gate
is not involved (same class as the chat assistant). The governance gate stays where it
belongs: on the governed dev run AFTER the UoW is linked.

## Scope

- `create_issue` returns only `html_url`; rather than change its signature (reuse, per the
  confine), `uow_publish` parses the issue number from the trailing path segment of the URL.
- The publish happy path (issue actually created) is covered at the link-step boundary
  (spine upsert + `link_work_item` + `/api/uows` resolution); the live `create_issue` HTTP
  call is exercised only with a real token (token-free tests cover the no-token rejection).

## Tests

`uow.rs`: `create_blank`, `append_authoring_turn` (chat order + requirements-prompt
stickiness), `link_work_item` (no re-key), and serde back-compat for the new fields.
`lib.rs`: `parse_author_response` (JSON / fenced / prose), blank-UoW creation + listing as
authoring, author endpoint appends chat without a token, publish rejected without a token,
and the publish link step links + resolves the work item without re-keying. All
`cargo test -p camerata-server -p camerata-ui` green; `cargo build --workspace` green.
