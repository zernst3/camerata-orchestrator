# Governed Development page rebuild (UI)

Date: 2026-06-21
Scope: `crates/ui` (cockpit + styles). No server/worktracker changes.

## What changed

The cockpit's "Governed Development" view was rebuilt from the ground up. The old
view (the story-spine left rail, `CockpitTopBar`, the five read-only stage tabs +
`StagePanel`, the inline run controls, the inspector rule list, and the
`AdoptFromGithub` / `DecomposeSection` blocks) is **replaced entirely** by a
provider-agnostic Work-Item + Unit-of-Work surface.

### New page layout (`GovernedDevPage`)

- **Left nav.** A top entry "Issue Management", then a list of UoW cards (one per
  UoW from `GET /api/uows`), each showing the referenced WorkItem title, its repo,
  and the UoW lifecycle stage. Selecting the top entry shows the issue-management
  panel; selecting a card shows that UoW's dev controls.
- **Issue Management panel** (`IssueManagementPanel`). A GitHub connection summary
  (provider + the active project's repos) and a "Pull work items" button
  (`POST /api/workitems/pull`). The pulled items render in a provider-agnostic
  `WorkItemTable` (columns: Repo, #, Title, State, Labels). Clicking a row opens a
  `WorkItemDetail` (full title + body + state + a link to the issue). Each row and
  the detail both expose a dedup-aware create/open affordance (`CreateOrOpenUow`):
  "Create Unit of Work from this issue" when none exists, "Open Unit of Work" when a
  UoW already references that work item.
- **UoW dev controls** (`UowDevControls`). Reuses the EXISTING governed-dev
  mechanisms keyed to the selected UoW: the gate self-check, the loop-guard control,
  "Run this work (governed)" (the gated run + model picker + agent-activity peek),
  the UoW panel, the live-run + provenance + explicit sign-off, and the clarify
  back-and-forth (`ClarifySection`). Adds an "Add comment to issue" box
  (`POST /api/workitems/comment`) and a "Pull latest work item" button
  (`POST /api/workitems/refresh`).

## Key decisions

1. **The gate is never bypassed.** The UoW "Run" path calls the same `start_run` /
   `fetch_run` polling loop the old page used; blocked starts surface the server's
   reason as a toast. Sign-off remains explicit (`RunProvenancePanel`).

2. **Provider-adapter seam.** The table, the UoW card list, the detail view, and all
   dev controls operate ONLY on the normalized `WorkItem` DTO (a stable cross-provider
   id, repo, number, title/body/state/url/labels). The *connection summary* and the
   *pull* are the only GitHub-aware pieces, isolated in `IssueManagementPanel`. A
   future Jira/Azure-DevOps adapter drops in a sibling connection/pull component that
   yields the same `WorkItem` shape; everything downstream is reused verbatim. The seam
   is marked with a comment block in `cockpit.rs`.

3. **UoW id is the dev-control key.** `GET /api/uows` returns `{ id, work_item, stage }`.
   The `id` doubles as the key the existing governed-dev endpoints accept (the server
   reconciles UoW id ↔ story id), so the reused components (`UowPanel`,
   `ClarifySection`, run/sign-off) address this UoW through its id with zero new client
   plumbing.

4. **Dedup by external ref is a display concern too.** `existing_uow_for` matches a
   work item's stable id against each UoW's referenced work-item id; `create_or_open_label`
   turns that into the right button text. Both are pure and unit-tested. The actual
   no-duplicate guarantee is server-side (`created=false` on `POST /api/uow/from-workitem`),
   which the UI honors by opening the returned UoW and toasting that one already existed.

5. **Pull is manual and uncached.** "Pull work items" hits the server each time; the
   table reflects exactly that pull. No client cache.

## Contract assumed (pending the server branch)

- `POST /api/workitems/pull`  body `{}` -> `{ items: WorkItem[] }`
- `GET  /api/uows`            -> `{ uows: [{ id, work_item, stage }] }`
- `POST /api/uow/from-workitem` body `{ work_item_id }` -> `{ uow_id, created }`
- `POST /api/workitems/refresh` body `{ work_item_id }` -> `{ item }`
- `POST /api/workitems/comment` body `{ work_item_id, body }` -> `{ ok, url }`

The reused dev-control endpoints (`/api/stories/:id/run`, `/api/runs/:id/*`,
`/api/uow/:id/*`, `/api/clarifications*`) are unchanged and keyed by the UoW id.

## Tests

Pure helpers are unit-tested in `crates/ui/src/cockpit.rs` (`mod tests`):
`work_item_state_badge`, `labels_summary`, `create_or_open_label`, `existing_uow_for`
(dedup display logic), and `WorkItem` deserialization from the contract shape.

## Cleanup

Removed the now-dead old-view code: `CockpitTopBar`, `StagePanel`, `AdoptFromGithub`,
`DecomposeSection` (+ `fetch_proposal` / `commit_children` / `fetch_children` /
`ProposedChildView`), `status_badge`, `active_stage_index`, `STAGE_TABS`,
`fetch_stories`, `fetch_rules`, `fetch_uow_map`, `fetch_open_clarifications`,
`fetch_github_issues` / `adopt_github_issue` / `urlencoding_encode` /
`IssueRow` / `IssuesResult` / `CockpitRule`, and the dead `DevStatus::badge_cls`.
`cargo check -p camerata-ui` is warning-clean.
