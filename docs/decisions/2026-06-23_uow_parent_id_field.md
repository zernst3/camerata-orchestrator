# Decision: UoW Parent ID field — author a child story under an existing issue

**Date:** 2026-06-23
**Status:** Implemented

## Context

The "author a new story with AI" blank-UoW flow creates a top-level GitHub issue at
publish time. Architects sometimes want the authored story to be a native GitHub
sub-issue (child) of an existing Epic or parent story. Without this, the only way to
nest the new story is a manual post-publish step in GitHub.

## Decision

Add a single optional `parent_id` field end-to-end through the blank-UoW flow:

1. **UoW storage** (`crates/server/src/uow.rs`): `UnitOfWork` gains
   `parent_id: Option<String>` with `#[serde(default)]`. An existing `uow.json`
   without the field deserializes without error (back-compat).

2. **Blank handler** (`POST /api/uow/blank`): accepts an optional JSON body
   `{ "parent_id": "42" }`. No body, `{}`, or `parent_id: null` all produce a draft
   with `parent_id = None` — existing callers are unchanged. The value is normalized
   at the handler (strip leading `#`, reject non-numeric) before storage.

3. **UI field** (`crates/ui/src/cockpit.rs`): a "Parent ID (optional)" text input
   appears above the "New Unit of Work — author a story" button. The entered value
   is threaded into `create_blank_uow(parent_id)` and sent as the body. Empty input
   sends `null`. The parent_id input is co-located with the action button — no extra
   navigation step.

4. **Publish linkage** (`POST /api/uow/:id/publish`, `crates/server/src/lib.rs`):
   after the child GitHub issue is created, if the draft's `parent_id` is set, the
   handler attempts to create a native GitHub sub-issue link via
   `POST /repos/{owner}/{repo}/issues/{parent_number}/sub_issues` with body
   `{ "sub_issue_id": <child_db_id> }`.

   Issue creation now uses `github_issues::create_issue_returning_id` (replaces the
   `onboard::create_issue` call) to return both the `html_url` and the GitHub database
   id (`id` field) required by the sub-issue API in a single call.

5. **Fail-soft**: if the sub-issue link fails for any reason (bad number, GitHub API
   error, permissions), the story is still published normally and the response
   includes a `parent_link_warning` field with the reason. The publish is never
   blocked by a failed parent link.

## Parent-id normalization

The function `github_issues::normalize_parent_number` strips a leading `#` and
validates that the remainder is all-digits. Inputs: `"42"` → `"42"`, `"#42"` → `"42"`,
`""` → `None`, `"abc"` → `None`. Applied at the blank handler; invalid values are
silently treated as None (no parent) so a typo cannot block draft creation.

## What is NOT changed

- The `DecompositionStore` is not touched.
- Re-keying the draft UoW after publish is still not done (draft-id-no-rekey stands).
- No parent_id UI is added to the publish step; it is set once at draft creation.
- The parent must be in the same `repo` that the story is published to (GitHub
  sub-issues are per-repo; the handler uses the publish `repo` for all GitHub calls).

## Files changed

- `crates/server/src/uow.rs` — `parent_id` field; `create_blank_with_parent`
- `crates/server/src/github_issues.rs` — `normalize_parent_number`,
  `create_issue_returning_id`, `fetch_issue_db_id`, `link_sub_issue`
- `crates/server/src/lib.rs` — `UowBlankReq`, updated `uow_blank` handler,
  updated `uow_publish` handler; 4 new tests
- `crates/ui/src/cockpit.rs` — `create_blank_uow(parent_id)`, parent_id input in
  `NewAuthoredUowButton`
