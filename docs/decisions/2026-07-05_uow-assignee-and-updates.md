# UoW assignee display, assign-to-me, and board-update notifications

Date: 2026-07-05
Status: Accepted (built).
Deciders: Zach (architect), Claude (architect)

Companion docs: [`USER_GUIDE.md`](../USER_GUIDE.md) section 6 (Governed Development).

## Context

A Unit of Work (UoW) references a GitHub issue (the "story") through its `WorkItem`.
Until now the UoW surface was a one-way read: you pulled an issue onto the spine and the
displayed metadata never moved again unless you hit **Pull latest** by hand. Two gaps
followed from that.

1. You could not see who owns the issue, and you could not claim it from Camerata. You had
   to go to github.com to assign yourself.
2. You could not tell when the board moved underneath you. If a teammate edited the issue,
   retitled it, closed it, or changed its assignee, the UoW silently showed stale data with
   no signal that a re-pull was warranted.

This change adds assignee visibility, a one-click "assign to me", and a quiet
change-detection poll with a re-sync affordance.

## Model additions

Two fields are threaded from the GitHub issue JSON all the way onto the `WorkItem` the
UoW carries:

- `assignees: Vec<String>`, the assignee **logins** (`assignees[].login` on the issue).
- `updated_at: String`, the issue's ISO-8601 last-updated timestamp.

They are parsed in `RawIssueWithState` into `IssueDetail` (`parse_issue_detail`) and copied
onto `WorkItem` in `WorkItem::from_github_issue`. Both are `#[serde(default)]` and default
to empty, so older serialized states round-trip.

They populate only on the single-issue refresh path (Pull latest / open). The bulk pull
(`IssueSummary`, list endpoint) and the canonical-story bridge (`from_canonical_story`) do
not carry them: the spine does not persist assignees, and the list endpoint intentionally
stays lean. This is why opening a UoW does a quiet one-shot refresh (below), to hydrate the
assignee list and set the update baseline.

## Three endpoints

- **`GET /api/me` returns `{ login }`.** The authenticated GitHub user for the configured
  token (`GET https://api.github.com/user`). Resolved **once** and memoized in `AppState`
  (`github_login_cache`), never per request. Returns `{ login: null }` gracefully when there
  is no token or the lookup fails. Only a **successful** login is cached, so setting a token
  later still resolves. This is the identity behind "assign to me".

- **`POST /api/workitems/assign` with `{ work_item_id, assignee }` returns
  `{ ok, assignees, updated_at }`.** Adds `assignee` (a login; the UI passes the current
  user's login for "assign to me") to the source issue via
  `POST /repos/:owner/:repo/issues/:number/assignees` with `{assignees:[login]}`. GitHub's
  assign response is the full updated issue object, so we return both the issue's
  **updated** assignee logins and its `updated_at` from that same response
  (`parse_issue_assign_outcome`). Needs the token (it errors without one, since there is
  nothing to assign against). GitHub treats assignment as additive and idempotent.
  `updated_at` is empty when GitHub's response happens to lack the field; the UI treats
  that as "no re-baseline available", which is safe (see "Assign re-baselines last-seen"
  below).

- **`POST /api/workitems/updated-check` with `{ items: [{ work_item_id, repo, number }] }`
  returns `{ updates: [{ work_item_id, updated_at, state }] }`.** The cheap "has anything
  changed?" probe behind the background poll. Degrades to `{ updates: [] }` with no token or on
  any per-repo failure, so a failed poll is silent.

### Rate-limit note (updated-check is batched, one list call per repo)

The naive implementation of updated-check is N single-issue GETs. Instead we **group the
requested items by repo and issue ONE list call per repo**:
`GET /repos/:owner/:repo/issues?state=all&sort=updated&direction=desc&per_page=100`, then
index the result by issue number. For a project with a handful of repos this is a handful of
calls per poll regardless of how many UoWs exist, which keeps us well clear of GitHub's REST
rate limits at a 60-second cadence.

Tradeoff: the `per_page=100` cap means a repo with more than 100 issues returns only its 100
most-recently-updated ones. An item whose number is **not** in that window is simply omitted
from `updates`; the poller then **retains its prior last-seen** (treats it as unchanged). For
a background "did the board move?" check this is the correct resilient default, because the
most-recently-updated issues are exactly the ones a change would surface in, and the user can
always **Pull latest** to force a single-issue re-fetch. `repo` and `number` are read from the
request body when present, else parsed from the `work_item_id`.

## Polling, last-seen, and change-flag design

The background poll lives in the Governed Development view (`GovernedDevPage`) as a
`use_future` loop that sleeps ~60s between ticks. **It holds no `LoadingGuard`**, because it
is a passive board check, not AI work, so it must never make the app read as "busy". It reads
the current UoW set each tick (so it always polls the live set), sends the
`(work_item_id, repo, number)` triples to updated-check, and folds each result into two
app-lifetime `GlobalSignal`s keyed by the work item id (`github:OWNER/REPO#N`):

- `UOW_LAST_SEEN: HashMap<work_item_id, updated_at>`, the baseline.
- `UOW_CHANGED: HashSet<work_item_id>`, the currently-flagged items.

The fold logic is a pure function (`fold_poll_update`) so it is unit-testable without the UI:

- **No baseline yet** for an id: **establish** it from this poll and do **not** flag. (So the
  first poll after a restart never marks everything changed.)
- **Polled `updated_at` strictly newer** than the baseline: **flag CHANGED**. ISO-8601 UTC
  timestamps compare correctly as plain strings, so no date parsing is needed.
- **Equal, older, or empty** polled value: no change.
- A poll **does not advance** the baseline. Only an open or a pull does, so a flag persists
  until the user actually syncs.

**Where the two change icons render:** a change icon shows in **both** places when an item is
in `UOW_CHANGED`. (a) On the UoW's left-nav card (`UowListEntry` render in `GovernedDevPage`),
and (b) in the UoW detail header (`UowDevControls`). Both are clear "updated on the board"
affordances.

**Clearing and baseline capture** is the other pure function (`clear_changed_and_bump`): it
removes the id from `UOW_CHANGED` and sets its `UOW_LAST_SEEN` to the freshly-pulled
`updated_at` (an empty timestamp clears the flag but never clobbers a good baseline). It is
called from three places:

- **On open.** `UowDevControls` does a quiet one-shot `refresh_work_item` on mount, which
  hydrates assignees and `updated_at` and sets the baseline plus clears any stale flag. (The
  component is keyed by UoW id, so switching UoWs remounts and re-baselines.)
- **Pull latest.** The existing button now also clears the flag and bumps the baseline.
- **The header's Updated affordance.** Same as Pull latest (re-fetch, clear, bump).
- **A successful assign** ("Assign to me" or any assign). See below.

### The poll is flag-only; assigning re-baselines last-seen (standing rule)

Two invariants hold for the whole feature, and this rule adds a third case that satisfies
them without weakening either:

1. **The poll never auto-updates displayed content.** It only ever flips the `UOW_CHANGED`
   flag on or off; it never mutates the `WorkItem` the UI shows. Seeing the story's fields
   change on screen always requires an explicit fetch (open, Pull latest, the Updated
   affordance, or a successful assign) — never a silent overwrite from the background tick.
2. **"Pull latest" stays the user's explicit choice to refresh displayed content.** Nothing
   in this change makes the poll implicitly pull; it only decides whether to show the
   change icon.
3. **Assigning a work item (including "Assign to me") re-baselines `UOW_LAST_SEEN` to the
   assign response's `updated_at`.** GitHub's assign response is the full updated issue, so
   assigning is itself an "update" from the poll's point of view — without this rebaseline,
   assigning yourself would make the very next poll tick flag the item CHANGED, which reads
   as a false "someone else touched this" notification when it was actually you, a second
   ago, through Camerata. `workitems_assign` forwards `updated_at`; on a successful assign
   the UI calls the existing `clear_changed_and_bump(work_item_id, updated_at)` (only when
   `updated_at` is non-empty) exactly as Pull latest does. A LATER real update — a strictly
   newer `updated_at` from someone/something else — still flags normally, because
   `fold_poll_update` only suppresses a change at or below the baseline it was just bumped
   to.

## Alternatives considered

- **Persist assignees and `updated_at` on the canonical story** so `/api/uows` carries them
  without a per-UoW refresh. Rejected for now: it widens the spine schema for a field the
  refresh path already provides, and the one-shot open refresh is cheap and also doubles as
  the baseline capture.
- **Per-issue polling** (a GET per UoW). Rejected on rate-limit grounds; the batched
  one-list-per-repo path is strictly cheaper and equally correct for change detection.
- **A visible or tabbed "updates" inbox.** Overkill for the signal we need; a per-UoW icon in
  the two places the user already looks is the minimal, legible affordance.

## Tests

- Server (pure): `parse_issue_detail` now asserts assignees and `updated_at` (and their empty
  defaults); `parse_authenticated_login`, `parse_issue_assignees`, `assign_payload`,
  `parse_issue_update_rows`. `parse_issue_assign_outcome_reads_assignees_and_updated_at` /
  `parse_issue_assign_outcome_updated_at_empty_when_absent` cover the assign response's
  `updated_at` parse (including the empty-when-absent fallback). Router: `/api/me`
  null-login, `/api/workitems/assign` token and empty-assignee guard,
  `/api/workitems/updated-check` token-less empty.
- UI (pure): `assignee_label`; `fold_poll_update` (first-sight baseline, newer flags,
  equal/older/empty do not flag, a poll does not advance the baseline); `clear_changed_and_bump`
  (clears and advances, empty timestamp still clears without clobbering).
  `self_assign_rebaselines_so_the_next_poll_does_not_self_flag` is the sequence test for the
  standing rule above: baseline established -> self-assign re-baselines via
  `clear_changed_and_bump` -> a `fold_poll_update` at that same timestamp does not flag -> a
  `fold_poll_update` at a strictly newer timestamp does flag. Network helpers via wiremock:
  `assign_work_item` request shape (now asserting the parsed `updated_at` too) plus `ok:false`
  maps to `None`, `fetch_me_login` present and null, `check_work_items_updated` row parse.
