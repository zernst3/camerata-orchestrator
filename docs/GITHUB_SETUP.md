# GitHub setup: linking Camerata to a real repo

All the GitHub plumbing is built and unit-tested against a fake transport. The one
remaining step is supplying a token so the real HTTP calls can run. This is the hard
blocker the rest of the app was built up to.

## What's already wired

- `GithubProvider` over `ReqwestTransport` implements the full `WorkItemProvider`
  (ingest, push-status, post-clarifying-questions, poll), unit-tested against a fake
  transport for request shape.
- The BFF selects the provider from the environment (`crates/server/src/provider.rs`):
  native (in-process) by default, GitHub when the env vars below are set.
- The clarify-bridge write-back, the `adopt` endpoint, and `/api/provider` all route
  through the active provider.
- `camerata worktracker-live` drives a real round-trip directly, for a quick link test.

## Environment variables

| Variable | Required | Meaning |
|---|---|---|
| `CAMERATA_GITHUB_TOKEN` | yes | A PAT (or GitHub App installation token) with Issues read + write on the repo. |
| `CAMERATA_GITHUB_REPO`  | yes | `owner/repo` (e.g. `zernst3/some-project`). |
| `CAMERATA_GITHUB_OWNER` | only if `CAMERATA_GITHUB_REPO` has no `/` | The repo owner. |
| `CAMERATA_GITHUB_ISSUE` | only for `worktracker-live` | The issue number to test against. |

## Token

A fine-grained PAT scoped to the one repo, with **Issues: Read and write**, is the
least-privilege choice. A classic PAT with `repo` scope also works. Create it at
GitHub → Settings → Developer settings → Personal access tokens. Never commit it; pass
it via the environment only.

## Quick link test (the first live call)

```
CAMERATA_GITHUB_TOKEN=ghp_xxx \
CAMERATA_GITHUB_REPO=you/your-repo \
CAMERATA_GITHUB_ISSUE=1 \
cargo run -p camerata -- worktracker-live
```

Expected: it ingests issue #1 as a story, posts a "Camerata live test" comment on it,
and polls for events. Check the issue on GitHub afterward; it should carry the comment.

## Running the app in GitHub mode

Set the two repo vars before launching, and the BFF (standalone or embedded in the
desktop app) switches onto the real repo with no code change:

```
CAMERATA_GITHUB_TOKEN=ghp_xxx CAMERATA_GITHUB_REPO=you/your-repo cargo run -p camerata-server
# or, for the desktop shell that embeds the BFF:
CAMERATA_GITHUB_TOKEN=ghp_xxx CAMERATA_GITHUB_REPO=you/your-repo cargo run -p camerata-ui
```

Confirm the wiring with `GET /api/provider` (it reports `"live": true` and the
`owner/repo` label). Then adopting a story (`POST /api/stories/adopt` with an issue id)
pulls a real issue, and posting a clarification on a story that has a GitHub ref also
posts a real comment on the issue.

## GitHub Projects v2 (board that spans repos)

A Projects v2 board sits ABOVE the repo: one board lists items from many repos
plus repo-less drafts. `camerata projects-live` reads a real board over GraphQL
and prints each item as a story with its SOURCE repo and BUILD TARGETS, so a
board drawing from several repos shows several distinct source repos in one
listing (the board-spans-repos capability).

| Variable | Required | Meaning |
|---|---|---|
| `CAMERATA_GITHUB_TOKEN` | yes | PAT with **read:project** (or `project`) scope plus repo read. Projects v2 needs the project scope on top of Issues. |
| `CAMERATA_GITHUB_PROJECT_OWNER` | yes | The user/org login that owns the board. |
| `CAMERATA_GITHUB_PROJECT_NUMBER` | yes | The project NUMBER (the integer in the project URL, e.g. `.../projects/3` → `3`), not the node id. |
| `CAMERATA_GITHUB_PROJECT_KIND` | no | `user` (default) or `org`. GraphQL roots the board under `user(login:)` vs `organization(login:)`. |

```
CAMERATA_GITHUB_TOKEN=ghp_xxx \
CAMERATA_GITHUB_PROJECT_OWNER=you \
CAMERATA_GITHUB_PROJECT_NUMBER=1 \
cargo run -p camerata -- projects-live
```

Expected: it lists every item on the board, each with its source (`Issue/PR <n>
in owner/repo`, or `draft (board-only)`) and its initial target repo, then prints
the count of DISTINCT source repos on the one board. Note: a classic PAT needs
the `project` scope; a fine-grained PAT needs the **Projects** permission, and
the board must be visible to the token's owner.

## What is verified vs what the token proves

- Verified now (no token): every adapter request SHAPE (method, URL, body) against the
  fake transport, plus the BFF defaults to native when the vars are unset.
- Proven only by the live run: that the real HTTP calls succeed against GitHub's API
  with your token and repo. That is the step that needs you.
