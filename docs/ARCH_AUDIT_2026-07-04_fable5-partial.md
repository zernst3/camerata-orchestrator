# Fable 5 audit (PARTIAL) — 2026-07-04

Point-in-time bug/gap audit run with Fable 5. The run was cut off by the 5-hour Fable
usage limit: the three top-level agents each fanned out into ~7-8 subsystem sub-agents
(~20+ concurrent Fable agents), which exhausted the budget quickly. Only two sub-agent
reports returned complete; the rest (and the three synthesis reports) were terminated
with no output. This file captures the salvage so nothing is lost. Findings are
Fable-reported and NOT yet independently verified unless noted.

## Coverage map

| Area | Status |
|---|---|
| UI: cockpit.rs | COMPLETE (7 findings) |
| UI: workspace.rs + table.rs | COMPLETE (8 findings) |
| Backend: symlink jail lead | PARTIAL (1 unverified lead) |
| Backend: server routes, UoW lifecycle, gate/enforcement, git/workspace, dev/PR runners, GitHub publish, rules/llm/credentials | NOT FINISHED (killed) |
| UI: cockpit/rules.rs, scan.rs, live_run.rs, uow.rs | NOT FINISHED (killed) |
| Gap analysis vs vision | NOT FINISHED (killed) |

## COMPLETE — cockpit.rs (7 findings)

- **F1 (high):** `AppUpdateBanner` fetches `GET /api/release`, which does not exist in the
  router (real route: `/api/updates/check`). Double mismatch: the UI expects top-level
  `{current, latest, update_available, release_notes}` but `updates_check` returns
  `{ok, app:{current_version, latest_version, update_available, release_url}, rule_drift}`.
  Banner is permanently dead with no error signal. The wiremock test mocks the
  non-existent `/api/release` with the UI's imagined shape, so it never caught it.
  cockpit.rs:2967-2974, banner 2979-3013.
- **F2 (medium):** ChatModelSetting seed race — two `use_resource`s (models registry vs
  `/api/settings`) race; if the registry resolves first, `selected` is seeded with the
  registry default and the persisted `chat_model` is skipped forever; touching the select
  then overwrites the saved choice. cockpit.rs:2679-2690.
- **F3 (medium):** Per-tab early-return templates are distinct `rsx!` trees, so the shared
  chrome (`AppUpdateBanner`, `CockpitNav`, `UsageMeter`) unmounts/remounts on every tab
  switch: usage meter flickers/reset + re-polls, banner `dismissed` resets (masked by F1
  today, becomes visible once F1 is fixed). cockpit.rs:2524-2626.
- **F4 (medium):** `export_project_json` writes the response body without checking HTTP
  status; a 404/500 still opens a Save dialog and writes an error/empty payload, returning
  true. Corrupt exports look successful. cockpit.rs:1598-1625.
- **F5 (low-medium):** ProjectsHome renders pending resource as "No projects yet" for up to
  ~2.5s on launch (None flattened to empty Vec). cockpit.rs:1791, 1896-1898.
- **F6 (low-medium):** Silent-failure affordances (no toast on failure): Create & open
  (1994-2003), Open (1961-1975), Import (2024), Export (1929-1933), Memory actions
  (645-664), Gate self-check erases prior verdict on failed probe (2246-2251).
- **F7 (low):** Create/import ignore `set_active_project` failure and enter the cockpit
  anyway, grounding on the wrong project. cockpit.rs:1998-2001, 2014-2019, 1829-1834.

Clean per this agent: refresh-signal wiring, one-shot guards, shared-context signals,
hierarchy schema mutations, loading flags, LoadingGuard invariant (no violations in file).

## COMPLETE — workspace.rs + table.rs (8 findings)

- **F1 (high):** "Start branch" success only sets `msg`; does not bump the shared `refresh`
  and cannot reach GitPanel's private `git_refresh`. Panel stays on the old branch, and
  Push/Pull read `current_branch` from the stale `branch_list`, so a later Push targets the
  OLD branch. workspace.rs:707-717, 1088-1141.
- **F2 (medium):** Dirty-warning + health pills go stale after "Commit all" (commit bumps
  only GitPanel's private `git_refresh`, not the RepoCard/health `refresh`). Page still says
  "Uncommitted changes" after a successful commit. workspace.rs:751-753, 1069-1074, 456-466.
- **F3 (low-medium):** Ship success re-fetches nothing; "N ahead" badge stays pre-push.
  workspace.rs:739-745.
- **F4 (medium):** "Clone / update all repos" discards `clone_project`'s Option result;
  failure ends the spinner and re-renders unchanged with no feedback. workspace.rs:567-571.
- **F5 (low):** "Export JSON" ignores the helper's bool; silent no-op on failure.
  workspace.rs:585-587, 373-399.
- **F6 (medium):** Drag-onto-a-branch cherry-pick ignores the target branch — every branch
  chip is a drop target, but the request is `{repo, sha}` and the server cherry-picks onto
  current HEAD. Dropping onto `release/x` while on `main` applies it to `main`. The hint text
  contradicts the behavior. workspace.rs:948-968; server lib.rs:7965 (`cherry_pick(&dir, &sha)`).
- **F7 (low):** No in-flight guard on cherry-pick; double-click/drop+click fires twice.
  Branch chips also aren't disabled while `branch_working`. workspace.rs:1189-1211, 950-968.
- **F8 (low):** `dragged_sha` is never cleared after a drop; `ondragover` always
  `prevent_default`s, so dropping unrelated draggable content onto a branch chip re-fires a
  cherry-pick of the stale SHA. workspace.rs:1177-1179, 955-956.

table.rs: clean (one documented latent limitation, not currently triggered).

## PARTIAL LEAD — backend (unverified)

- Committed **symlinks inside a worktree may defeat the lexical jail check**:
  `normalize_lexical` deliberately skips symlink resolution while `fs::write` follows them,
  so a symlink committed into a repo could point a gated write outside the jail. Needs
  verification against the gate's actual write path.

## Next steps

- Resume the unfinished subsystems after the Fable limit resets (~1pm), OR verify + fix the
  above with a non-Fable model now (the 15 findings are already a concrete work list).
- If resuming with Fable, cap the fan-out: launch ONE agent per subsystem sequentially or a
  bounded parallel set, not agents-that-spawn-agents, to avoid the budget blowout.
