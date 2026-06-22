# UoW governed-dev redesign — chatbot-grounding doc update

**Date:** 2026-06-21

## What changed and why

`docs/USER_GUIDE.md` and `docs/TECHNICAL.md` are baked into the in-app assistant's system prompt
at compile time (`include_str!` in `crates/ui/src/chat.rs`). Both described the pre-redesign UoW
flow and would have caused the assistant to give wrong answers about how governed development works.

## USER_GUIDE.md — sections changed

**§1 (Projects / portability).** Added a "Project config vs. project data" note explaining that
exports carry only transferable config (repos, ruleset, onboarded state, tier map). UoWs, the story
spine, onboarding drafts, and local repo paths are local to each developer and are never included in
an export. Source: `docs/decisions/2026-06-21_project_config_vs_data_separation.md`.

**§6 (Governed Development — UoW dev controls).** The stale "Run this work (governed)" standalone
button and the "Lifecycle strip + transitions" / "Ask the team" bullet list are replaced with an
accurate step-bound run flow:

- All AI runs are bound to the UoW lifecycle stage; no standalone run button exists.
- **Intake:** single model select + **▶ Begin investigation** (one gated investigation agent; stage
  advances to Investigating on start).
- **Investigating:** **Approve decisions** transition (server gates the next step until all decision
  records are approved).
- **Decisions Approved:** Strongest/Balanced/Fast per-tier model selects + **▶ Run development
  (governed)** (three-tier orchestrator-led fleet; strongest agent leads and delegates via the
  governed `mcp__camerata__delegate` tool; all tiers gated; depth limited to 1; escalation
  parent-driven).
- "Ask the team" panel is removed; loop a teammate in via the **Add comment to issue** @-mention box.

**"The whole loop" summary.** Updated to reflect the step-bound investigation + decisions-approved +
development phases instead of the stale "run governed work" phrasing.

## TECHNICAL.md — sections changed

**§10 (Unit of Work).** Three sub-sections added after the existing `UowStore` paragraph:

1. **Config vs. data storage separation** — table showing which stores travel in export vs. stay
   local; rationale for keeping UoWs local.
2. **Investigation run (`POST /api/uow/:story_id/begin-investigation`)** — request/response shape,
   stage-guard behavior (409 if not at Intake), model resolution chain, the single-agent
   `investigation_run.rs` runner (same gate machinery as the fleet, read-oriented, no scaffold),
   live-vs-scripted split.
3. **Tiered development run + governed `delegate` MCP tool** — `execute_live_run_tiered` / plan
   shape / `build_from_plan_with_tier_map` wiring; orchestrator-mode selection (lead = first
   Strongest task); `delegate` tool handler (depth guard → tier resolution → child session without
   `DELEGATE_ENABLED` → `ClaudeCliDriver` with `orchestrator = false` → sync spawn); the two
   independent depth-1 guarantees (structural via `--allowedTools` exclusion + explicit counter);
   parent-driven escalation (`INCOMPLETE:` signal); gate preservation proof (Task disallowed for
   all agents including the orchestrator; every child born gated).

**§11 (Cockpit UI — Governed Development page).** The `UowDevControls` description updated to
reflect the step-bound run surface: `UowStepRunControls` (lifecycle strip + per-phase controls),
the removed "Ask the team" panel, and the new **Add comment to issue** @-mention box.

## Stale text confirmed removed

- The standalone "Run this work (governed)" bullet is gone from USER_GUIDE.md §6.
- The transition-only "Begin investigation" bullet (which described the button as only advancing
  the stage, not running a real agent) is gone and replaced with the accurate investigation-run
  description.
- "Ask the team (the human↔AI clarify loop)" bullet is gone; replaced by the @-mention comment box.

## Claims verified against code

All claims were verified against:
- `crates/ui/src/cockpit.rs` — `UowDevControls`, `UowStepRunControls`, lifecycle strip rendering,
  tier selects, `dev_run_body`, `start_dev_run`, `begin_investigation_run`.
- `crates/server/src/lib.rs` — `start_run` handler, `uow_begin_investigation` handler,
  `StartRunReq` with `tier_map`, gate enforcement before both paths.
- `crates/server/src/live_fleet.rs` — `execute_live_run_tiered`, plan shape, `build_from_plan_with_tier_map`.
- `crates/fleet/src/lib.rs` — `build_from_plan_with_tier_map`, orchestrator session, lead index.
- `crates/gateway/src/main.rs` and `src/delegate.rs` — `delegate` tool handler, `OrchestratorConfig`,
  depth guard, child session construction.
- `crates/agent/src/lib.rs` — `DELEGATE_TOOL`, `allowed_tools_for_role_with_mode`, `as_orchestrator`,
  `disallowed_builtins` (Task listed for every agent including orchestrator).
- `docs/decisions/2026-06-21_project_config_vs_data_separation.md` — config vs. data boundary.
- `docs/decisions/2026-06-21_uow_be_increment1.md` and `2026-06-21_uow_delegate_tool_increment2.md`.

## One unverified claim

The UI comment in `cockpit.rs` mentions that the `Investigating` stage transition
(`Approve decisions`) 409s with a "precise reason" when not all decisions are approved. The 409
response and the no-code-first gate enforcement are verified in the server tests
(`start_run_is_blocked_until_decisions_are_approved`). The exact error-message wording displayed in
the UI toast was not audited separately — the docs describe the behavior ("server enforces this gate;
returns 409 if not all are approved") without quoting the exact toast string, so there is no
unverified verbatim claim.
