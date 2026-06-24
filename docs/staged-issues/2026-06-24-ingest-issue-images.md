# Feature: ingest issue images — download/cache attached + inline images, make them usable by the AI and the UI

> **Status: STAGED → being filed on the 2026-06-24 GitHub push.**
> Create as a **sub-issue of the Intake epic.** Issues only (not PRs) for now.

**Title:** Ingest issue images (inline + attachments): download, project-scoped cache, render auth'd in UI, pass as image content to the model

---

## Problem

Ingesting a GitHub issue captures **text only**. Image markdown (`![alt](url)`) and `<img src>` in the body are stored **verbatim as URL strings** in `CanonicalStory.description` (`crates/server/src/github_issues.rs:461`); **no image bytes are ever downloaded** — ingestion is fetch-JSON → parse → store. At display the stored markdown is rendered to HTML (`crates/ui/src/cockpit/uow.rs:1620`) and the **browser** fetches each URL live.

Two real gaps:
1. **Private-repo attachments 403 in the UI.** GitHub `user-attachments` / `assets` URLs are auth-scoped; the browser has no GitHub auth context, so attached images on private repos fail to render.
2. **The orchestration AI never sees the image.** The model only ever receives the URL string, never the bytes. A mockup, screenshot, error photo, or diagram attached to a story is **invisible to the agent that is supposed to implement it** — a meaningful correctness gap for a governed-AI-dev tool.

## Proposed

1. **Parse** the issue body (and comments) at ingest for image references: markdown `![alt](url)`, HTML `<img src>`, and bare `github.com/.../user-attachments|assets/...` URLs.
2. **Download the bytes with the authenticated GitHub client** (the same token already used to fetch the issue) so private attachments are retrievable.
3. **Store them project-scoped** (must honor the strict-isolation invariant — assets keyed under the owning project, never shared across projects), and map each original URL → cached asset.
4. **Serve them auth'd in the UI** so they render regardless of browser GitHub state (rewrite the rendered body to the local/cached asset, or proxy).
5. **Pass them as image content blocks to the model** when the story drives AI dev, so the agent actually sees the attached visuals.

## Acceptance

- A private-repo issue with an attached screenshot ingests with the image bytes captured (not just the URL).
- The image renders in the work-item view without the viewer needing GitHub auth.
- The model receives the image as visual content when the story is used for implementation.
- Assets are project-scoped (a different project never sees them).

## Scope

`crates/server/src/github_issues.rs` (parse + authenticated download), a project-scoped asset store + serve route, `crates/ui/src/cockpit/uow.rs` (render cached asset), the intake → model path (attach image content). **Issues only** (PRs out of scope for now). Parent: **Intake epic.**
