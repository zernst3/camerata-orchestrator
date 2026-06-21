# Governed Development: provider-agnostic WorkItem + UoW server layer

Date: 2026-06-21
Status: Accepted
Scope: `crates/server` (the Axum BFF) on top of the `camerata-worktracker` port.

## Context

The rebuilt Governed Development surface needs a provider-agnostic work-item layer the
UI can consume without knowing about GitHub, Jira, or the worktracker `CanonicalStory`
internals. The old flow was the inline owner/repo "adopt-issue" hack
(`POST /api/stories/adopt-issue`): the UI named a repo and an issue number directly and
got a `CanonicalStory` back. That couples the UI to repo coordinates and to the
canonical-story shape, and it does not project the adopted item onto a Unit of Work
(UoW) or wire it to the governed-dev gate.

This decision introduces a normalized `WorkItem` DTO at the API boundary, mapped from
the worktracker `CanonicalStory` + its source repo, plus the endpoints that pull work
items across the active project's repos, project one onto a UoW (deduped by external
ref), refresh one, and comment back to the source. The UoW dev controls (run / clarify
/ sign-off) REUSE the existing governed-dev endpoints, keyed by the UoW's story id — the
gate is never bypassed.

The full `CanonicalStory` → `WorkItem` rename across ~95 references is deliberately NOT
done now (see Followups). The DTO is introduced at the API boundary and bridged.

## Shared contract (server emits, UI consumes)

### `WorkItem` DTO

```json
{
  "id": "github:OWNER/REPO#123",
  "provider": "github",
  "repo": "OWNER/REPO",
  "number": 123,
  "title": "string",
  "body": "string",
  "state": "open" | "closed",
  "url": "https://github.com/OWNER/REPO/issues/123",
  "labels": ["string", ...]
}
```

- `id` is the STABLE, provider-namespaced identity: `github:OWNER/REPO#NUMBER`.
- `repo` is set on EVERY item (the contract requires each item to carry its repo, since
  a pull spans all the active project's repos).

### Identity bridge (WorkItem id ↔ UoW story id)

The UoW layer keys by `story_id`. The bridge strips the `github:` provider prefix:

```
work_item_id  = "github:OWNER/REPO#123"
story_id      = "OWNER/REPO#123"   (== the canonical-story spine id, == the adopt-issue id)
```

This keeps a UoW created from a work item interoperable with the rest of the spine and
makes dedup-by-external-ref a pure string identity on `work_item_id`. Implemented in
`crate::workitems::{work_item_id_to_story_id, story_id_for}`; the address parser
`parse_github_work_item_id` returns `(repo, number)` for the GitHub API calls.

## Endpoints

| Method + path                  | Body                          | Response                                          |
|--------------------------------|-------------------------------|---------------------------------------------------|
| `POST /api/workitems/pull`     | `{}`                          | `{ "items": WorkItem[] }`                          |
| `GET  /api/uows`               | —                             | `{ "uows": [{ "id", "work_item": WorkItem\|null, "stage" }] }` |
| `POST /api/uow/from-workitem`  | `{ "work_item_id" }`          | `{ "uow_id": string, "created": bool }`            |
| `POST /api/workitems/refresh`  | `{ "work_item_id" }`          | `{ "item": WorkItem }`                             |
| `POST /api/workitems/comment`  | `{ "work_item_id", "body" }`  | `{ "ok": bool, "url": string }`                    |

Behaviour notes:

- **pull** uses the ACTIVE project. It lists ALL open issues across ALL of the active
  project's repos via the GitHub adapter (`github_issues::list_open_issues`, which reuses
  the worktracker `ReqwestTransport`), normalizes each to a `WorkItem` with its repo set,
  and returns the union. Manual (user-triggered), no cache. Degrades gracefully: with no
  token / no active project / no repos it returns `{ "items": [], "message": ... }`
  (never an error). A per-repo fetch failure is skipped, not fatal — the union of the
  repos that resolved is returned.
- **uows** resolves each UoW's referenced `WorkItem` from the story spine (`work_item` is
  `null` for native/legacy stories with no external ref) and reports the lifecycle
  `stage` as a snake_case wire string (`intake`, `investigating`, `decisions_approved`,
  `development`, `awaiting_qa`, `signed_off`).
- **from-workitem** DEDUPES by external ref: if a UoW already exists for the work item's
  story id it returns `{ created: false }` with the existing id — never a duplicate. On
  first creation it ensures the work item is on the canonical spine (idempotent upsert:
  refreshed from GitHub when a token is set, else a minimal row seeded from the id) so
  `/api/uows` resolves it and the governed-dev endpoints have a story to run against.
- **refresh** re-pulls one item via `github_issues::get_issue_detail` (carries state +
  labels) and maps it to a `WorkItem`. Needs the token.
- **comment** posts a plain markdown comment onto the source issue via
  `github_issues::comment_on_issue` (payload `{ "body": <markdown> }`), returning the
  created comment's `html_url`. Needs the token. Distinct from the worktracker provider's
  structured `push_status` / `post_clarifying_questions`, which wrap content in a status
  rollup or a clarify marker; a free-text dev-surface comment uses the plain primitive.

## UoW dev controls reuse the existing gate

The run / clarify / sign-off controls REUSE the existing governed-dev endpoints
(`/api/stories/:id/run`, the clarify endpoints, `/api/runs/:id/sign-off`, and the
`/api/uow/:story_id/*` lifecycle transitions), keyed by the UoW story id derived from
the work item. No new run/clarify/sign-off endpoint was added and the gate is not
bypassed.

## What was built (additive)

- `crate::workitems` (new module): the `WorkItem` DTO, `from_github_issue` /
  `from_canonical_story` mappers, and the id bridge.
- `crate::github_issues` (additive helpers only): `parse_single_issue`,
  `parse_issue_detail` + `IssueDetail`, `get_issue_detail`, `comment_on_issue`.
- `crate::lib`: the five routes + handlers above, plus `github_token()`,
  `parse_github_work_item_id`, and the `UowView` response shape.

The retired hack (`POST /api/stories/adopt-issue`, `GET /api/github/issues`,
`POST /api/stories/adopt`) is left in place for now and continues to work; the new layer
is the replacement path the rebuilt UI should target. Removing the hack is a followup
once the UI cuts over.

## Tests

`cargo build --workspace` + `cargo check --workspace` green; `cargo test -p
camerata-server` green (433 lib tests). New coverage:

- pull normalization (graceful no-token degrade; repo set + fields covered in
  `workitems::tests::from_github_issue_sets_repo_and_all_fields`),
- from-workitem dedup (`uow_from_workitem_dedups_by_external_ref`),
- uows view carries work item + stage (`uows_list_carries_workitem_and_stage`),
- comment input validation + work-item id addressing
  (`workitems_comment_validates_input`, `parse_github_work_item_id_extracts_repo_and_number`),
- the pure mapping + id bridge unit tests in `crate::workitems`,
- the new `github_issues` parse helpers.

## Followups

1. Full `CanonicalStory` → `WorkItem` rename across the ~95 references (the worktracker
   spine, providers, sync engine, and the server handlers that still speak
   `CanonicalStory`). This decision only bridges at the API boundary.
2. Remove the retired adopt-issue hack (`/api/stories/adopt-issue`,
   `/api/github/issues`, `/api/stories/adopt`) once the UI is fully on the WorkItem
   layer.
3. Multi-provider: `from-workitem` / `refresh` / `comment` currently special-case the
   `github:` prefix. Generalize the id parser + the I/O dispatch when a second provider
   (Jira / ADO) is wired onto this layer.
4. Echo suppression / status write-back on the comment path: the plain comment does not
   record an expected echo. When inbound polling is wired into this surface, route the
   dev-surface comment through the sync `ExpectedEchoTable` so it is not re-ingested.
