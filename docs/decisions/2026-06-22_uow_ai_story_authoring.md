# Author a story from a blank UoW with AI, push to the board, auto-link

**Date:** 2026-06-22 · **Decided by:** Zach. The inverse of `from-workitem`: start with a UoW and
*author* the issue with AI, instead of starting from an existing issue.

## Flow

1. **Create a blank UoW** (no story yet) — a "New Unit of Work / author a story" action.
2. **Prompt the requirements** (free text) inside the UoW.
3. **AI drafts a story** (title + body) from the prompt, with a **back-and-forth clarification chat**
   — the AI asks clarifying questions, the user answers, the draft refines. (Story authoring CAN
   have AI assistance; this is the product-owner loop.)
4. **Push to the board** — pick a target repo (one of the project's repos), create a GitHub Issue
   from the drafted story (reuse `onboard::create_issue`), get the new issue number.
5. **Auto-link** — set the UoW's work-item reference to the newly-created issue. The UoW becomes a
   normal story-linked UoW; dev runs proceed as usual.

## Data model

- A blank UoW is keyed by a **draft id** (e.g. `draft-<uuid>`) with `work_item = None` and a
  story-authoring state (the requirements prompt + the clarification chat transcript + the current
  draft title/body). The per-UoW endpoints already key by an opaque story_id, so the draft key works
  as the key for runs/lifecycle; the build chooses whether to keep the draft key after linking or
  migrate to the real `owner/repo#num` (keeping it is simpler and avoids re-keying — the work-item
  ref carries the real issue).
- Provider-agnostic core + GitHub adapter: the push is a GitHub `create_issue` now; Jira/ADO are
  future adapters. The link is the same external-ref mechanism `from-workitem` uses.

## Gate / scope notes

- Story authoring is an **LLM text-generation assist** (drafts/refines the story) — NOT a
  code-writing agent. No `gated_write`, the gate is not in this path (same class as the chat
  assistant). Reuse the existing LLM machinery + a chat loop.
- This is a **product-owner tool** (author requirements → AI story → board) but useful to any
  Camerata user who wants to start work without a pre-written ticket.

Relates to [[workitem_uow_governed_dev_architecture]] (WorkItem = the normalized story; this just
authors one before linking).
