# Chat grounds on the project the user is viewing

**Date:** 2026-06-24
**Branch:** fix/active-project-follows-view

## Problem

The chat assistant grounds on the *active project* (server-side state set via
`POST /api/projects/active`), but opening a project in the cockpit did not
always update that server-side state. Three paths in `ProjectGate` transitioned
to `CockpitScreen::InProject` without calling `set_active_project` first:

- **Create and open**: `create_project` returned a `ProjectView` but the id
  was not passed to `set_active_project`.
- **Fresh import**: `ImportResult::Imported(_)` discarded the project id.
- **Overwrite-import confirm**: same discard.

The "Open" button path already called `set_active_project` correctly. So when
the user opened project X with the "Open" button the state was correct, but the
three paths above left whatever project was previously active as the active one.

**Symptom:** user runs a scan in Camerata while agora-new is the auto-set active
project. Chat shows agora-new's state and an empty scan section instead of
Camerata's scan.

## Decision: fix A — every cockpit project-open path calls set_active_project

Every transition to `CockpitScreen::InProject` that opens a NEW project
(as opposed to re-entering one that was just set active) must call
`set_active_project(&project_id).await` first. The id is available at the
click site in all three paths — it just was not being used.

The resume-prompt paths ("Start over" / "Continue where you left off") are
exempt: they execute only AFTER the user clicked "Open", which already called
`set_active_project`. There is no second call needed there.

## Decision: fix B — selected rules and options are always in the chat context

The user must be able to ask "which rules / options did I select?" at any point
during the onboarding flow — including BEFORE any scan. The selections live in
the onboarding draft (`repo_selection`, `chosen`) and are auto-saved server-side
as soon as the user checks a box.

### Server: `render_selected_rules_for_chat` (pure helper)

A new pure function reads `repo_selection` + `chosen` from the draft JSON,
enriches rule ids with titles and option labels from `scan.proposed_rules`, and
formats a compact block:

```
Total selected: 4 rule(s) across 2 repo(s)
Rules:
  SEC-NO-HARDCODED-SECRETS-1 · "No hardcoded secrets" (all repos) [default option]
  PERF-FK-INDEX-1 · "FK index required" (owner/api) → chosen: strict-mode
```

Returns `None` when `repo_selection` is absent or empty, so the field is omitted
from the JSON response (skipped via `#[serde(skip_serializing_if = "Option::is_none")]`).

`selected_rules_section: Option<String>` added to `ProjectContextResponse` and
populated in all three branches of `active_project_context`:

- **No active project**: `None`
- **Post-onboard**: populated from draft (draft may carry the last onboarding selection)
- **Pre-onboard with draft**: populated (this is the primary pre-scan path)
- **Blank (no draft)**: `None` (nothing to populate from)

### UI/chat: Layer 3d

`ProjectContextLite` extended with `selected_rules_section`.

The two separate resource fetches (scan + rules) were collapsed into a single
`fetch_project_context_sections` that returns `(scan_section, selected_rules_section)`
in one round-trip, avoiding a redundant HTTP call.

`unified_system_prompt` gains a 6th parameter `selected_rules_section: Option<&str>`
and injects it as Layer 3d immediately after Layer 3c:

```
=== LAYER 3d: SELECTED RULES & OPTIONS (this project, from onboarding draft) ===
Total selected: 3 rule(s) across 1 repo(s)
Rules:
  ...
```

The affordance panel ("What this assistant can see") gains a new line:
- `● Selected rules (N selected)` (green dot when present)
- `● Selected rules (none yet)` (grey dot when absent)

Because the chat refetches context per session open, the selected-rules section
reflects the user's current saved selections whenever they ask.

## Alternatives considered

- **Only show selected rules post-scan**: rejected. The user selects rules DURING
  the onboarding flow, and the primary use-case for "which rules did I select?" is
  BEFORE any scan completes.
- **Separate endpoint for selected rules**: rejected. The data is already available
  in the active-project-context endpoint; a separate endpoint would add an extra
  round-trip for no benefit.
- **Embed in existing `draft_json` field**: rejected. `draft_json` is only populated
  in the PreOnboard phase and is the full raw draft (noisy). The selected-rules
  section is a focused, readable extract that works across all phases.

## Files changed

- `crates/ui/src/cockpit.rs` — Piece A: three project-open paths wired to
  `set_active_project`
- `crates/server/src/lib.rs` — Piece B: `render_selected_rules_for_chat`,
  `ProjectContextResponse.selected_rules_section`, all four branches updated
- `crates/ui/src/chat.rs` — Piece B: `ProjectContextLite`, `fetch_project_context_sections`,
  `unified_system_prompt` Layer 3d, affordance panel, all 6th-arg call sites
