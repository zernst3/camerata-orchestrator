# Dev-cycle observability — surface the real events during a governed run

**Date:** 2026-06-22 · **Decided by:** Zach. The "development" action should not be a black box —
show what's happening underneath: what's passed back and forth + what's checked. Concise, NOT
verbose (no agent chain-of-thought).

## Events to surface (the meaningful ones)

- **Gate decisions** (per `gated_write` attempt): ALLOWED / DENIED + target + rule-id + reason. The
  DENY → bounce-back-to-agent is the key one.
- **Tiering / delegation**: which tier/model each agent runs on; delegate dispatched (subtask →
  tier); delegate returned (incl. `INCOMPLETE:` escalation).
- **Layer-2 checks**: started → passed/failed (+ violated rule-ids); each bounce-and-revise
  iteration (failed → sent back to the agent → re-checked).
- **Stage transitions** (the lifecycle moving).

NOT included: the agent's token-by-token thinking. Only what's exchanged + checked.

## Plumbing (reuse existing infra)

There is already a `GateEvent` model + `RunStore::push_event` + a run event stream the UI renders
(today driven by the SCRIPTED `run_event_script` on the token-free path) and the fleet's
`BuildEvent` (stage/`bounced`). The build wires the LIVE path's REAL events into that stream:
- **Gateway**: append structured gate-decision records (allow/deny + target + rule + reason + ts)
  to a per-session events JSONL (derived from the session/rules-file dir). Keeps the existing
  stderr line; ADDS the structured sink. (Observability only — does NOT change enforcement.)
- **Fleet / coordinator**: emit richer events — layer-2 check start/result (violated rules) + the
  bounce iteration; the delegate tool's dispatch/return; the tier/model per spawned agent. Extend
  `BuildEvent` kinds.
- **Server (live executor)**: fold the gateway's gate-decision file + the fleet's BuildEvents into
  the run's event stream via `push_event`, so the run-poll returns them live.
- **UI**: a concise "Development activity" log (extend the existing event view) shown during the
  dev run — gate allow/deny, tier/delegate, layer-2 pass/fail + bounce, stage.

## Gate note
This is OBSERVABILITY (recording + surfacing decisions). It does NOT change what the gate
allows/denies — the deny-before-execute floor + the bounce-and-revise loop behave exactly as before;
we just make them VISIBLE. Layer-1 + the gate's enforcement are untouched.

Relates to [[camerata_gate_universal_enforcement]] and the UoW dev-run architecture.
