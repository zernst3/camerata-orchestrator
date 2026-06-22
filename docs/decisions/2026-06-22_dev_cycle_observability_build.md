# Dev-cycle observability — the build

**Date:** 2026-06-22 · Implements [`2026-06-22_dev_cycle_observability.md`](2026-06-22_dev_cycle_observability.md).

The governed "development" run is no longer a black box. A LIVE run now surfaces the
real dev-cycle events as a concise activity log in the cockpit: gate ALLOW/DENY (with
rule + reason, incl. the DENY → bounce-back), layer-2 check pass/fail (+ violated rules)
and each bounce-and-revise iteration, the tier/model each agent runs on, delegate
dispatch/return (incl. `INCOMPLETE:` escalation), and stage transitions. No agent
chain-of-thought — only what is exchanged and checked.

This is OBSERVABILITY ONLY. It adds event recording + surfacing; it changes nothing
about enforcement (see "Enforcement unchanged" below).

## 1. Gateway → structured gate-decision sink

`crates/gateway/src/main.rs`. Alongside the existing `[gateway] gated_write
gate_decision=…` stderr line (kept verbatim), each `gated_write` now appends a structured
record to a per-session JSONL sink:

- `GateDecisionRecord { kind, verdict, target, rule, reason, ts_ms }`, one JSON object
  per line. `kind` is `#[serde(default)]` (`"gate"`), so older readers/payloads are
  wire-compatible.
- `build_gate_record(target, decision, ts_ms)` is a PURE classifier over the decision
  string the handler already produced (`ALLOWED…` → allow; `DENIED [rule]…` → deny with
  the bracketed rule, incl. the `JAIL` tag). It contains zero decision logic — it records
  what was already decided.
- Sink path: `CAMERATA_GATE_EVENTS_FILE` if set, else a `gate-events.jsonl` sibling of
  `CAMERATA_RULES_FILE` (so the orchestrator that wrote the rules file knows where to
  tail). Unset → no sink (standalone/test runs keep just the stderr trace).
- The `delegate` tool also appends two records: `kind:"delegate-dispatch"` (subtask →
  tier) before the spawn, and `kind:"delegate-return"` after (verdict `returned`, or
  `incomplete` when the child's output starts with `INCOMPLETE:`). The spawn gate
  (orchestrator-mode + depth guard) is decided exactly as before; these only RECORD it.

Writes are best-effort: a sink failure is ignored (the stderr line is authoritative) and
never affects the tool's return value.

## 2. Fleet / coordinator → richer `BuildEvent`s

`crates/fleet/src/lib.rs`. `BuildEvent` gains three variants (the existing
`Scaffolding`/`StageStarted`/`StageFinished`/`Verifying`/`Done` are unchanged):

- `AgentTier { index, role, model, is_lead }` — the tier/model each spawned agent runs
  on, and whether it is the lead/orchestrator. Emitted right after `StageStarted` in both
  the single-model and tiered build paths (single-model: the operator-wide model, no
  lead; tiered: the per-stage model resolved from the `TierMap`, lead = the strongest
  stage).
- `Layer2Result { index, total, passed, violated_rules }` — the layer-2 check result per
  stage.
- `ReviseIteration { index, violated_rules }` — a bounce-and-revise pass ran for a dirty
  stage.

`Layer2Result` / `ReviseIteration` are derived AFTER the fleet runs, from the already-
decided `FleetReport` (`emit_stage_reports`, a pure helper shared by both build paths):
a stage that `bounced` emits a `ReviseIteration` citing its `initial_violations`, then a
`Layer2Result` (`passed` = `final_violations` empty). The coordinator's bounce loop and
loop-guard cap are untouched — these read the report, they do not drive the loop.

Delegate dispatch/return is NOT a `BuildEvent` (it happens inside the gateway subprocess,
which the fleet can't observe); it rides the gateway's JSONL sink instead (§1).

## 3. Server live path → fold into `Run.events`

`crates/server/src/live_fleet.rs`. The live executors (`execute_live_run`,
`execute_live_run_tiered`) now:

- `start_gate_observability(...)`: create a fresh per-run `gate-events.jsonl` under the
  run's temp root, point `CAMERATA_GATE_EVENTS_FILE` at it (process env → inherited by
  the `claude` CLI and the gateway it launches, including delegate children), and spawn a
  `tail_gate_events` task that polls the file and folds each new record into the run via
  `push_event`. A shared `Arc<AtomicUsize>` seq is used by both the tailer and the
  build-event callback so events interleave with coherent ordering. `stop_gate_
  observability` signals done + awaits a final drain after the build completes.
- `gate_record_to_event(seq, rec)` — PURE map of a JSONL record to a `GateEvent`:
  `kind:"gate"` → layer `"layer-1"` (allow/deny + rule); `delegate-*` → layer
  `"delegate"`.
- `build_event_to_gate_event(seq, event)` — PURE map of each `BuildEvent` to a
  `GateEvent` with an appropriate `layer` + `verdict`: `AgentTier` → `tier`/`info`;
  `Layer2Result` → `layer-2`/`pass|fail` (+ rules); `ReviseIteration` → `layer-2`/
  `revise`; `StageStarted/Finished` → `stage`; `Done` → `checks`/`allow|deny`;
  `Verifying` → `None` (a status flip, not an event).

So a LIVE run now populates `Run.events` with the real activity; previously only the
scripted (token-free) path did.

`GateEvent` was NOT extended with a new field: the design's preferred reuse of
`layer` + `verdict` + `detail` was sufficient, and it avoids editing the ~25 existing
`GateEvent { … }` construction sites (several outside this change's confine).

## 4. UI → development-activity log

`crates/ui/src/cockpit.rs` + `crates/ui/src/style.rs`. `RunGateEvent` gains a
`#[serde(default)] layer` field (legacy/scripted payloads without it deserialize
unchanged). `live_event_style(layer, verdict)` is a pure label+CSS-class mapper giving
each observability kind a distinct tag/colour: `GATE DENY`/`GATE ALLOW`, `LAYER-2 PASS`/
`LAYER-2 FAIL`/`REVISE`, `TIER`, `DELEGATE`/`DELEGATE INCOMPLETE`/`DELEGATE RETURN`,
`CHECKS PASS`/`CHECKS FAIL`, `STAGE`. `LiveRunPanel` renders `Run.events` through it as
the "Development activity" log (caption added), shown during the run above the sign-off /
provenance section and the AgentActivity drawer. Concise rows only — no chain-of-thought.

New CSS classes: `.live-event.revise`, `.live-event.delegate`, `.live-event.tier`,
`.live-events-caption`.

## Enforcement unchanged

The gate allows/denies exactly as before, the bounce-and-revise loop behaves exactly as
before, `Task` stays disallowed, and every agent stays gated. The only additions are
event recording (the gateway sink + the new `BuildEvent` kinds derived from the existing
report) and surfacing (the server fold + the UI log). All decision/classification helpers
(`build_gate_record`, `gate_record_to_event`, `build_event_to_gate_event`,
`emit_stage_reports`, `live_event_style`) are pure translations of decisions already made
elsewhere. The existing gateway decision tests (jail, delegate depth/tier, evaluate)
still pass.

## Tests (token-free)

- Gateway: `build_gate_record` allow/deny/jail classification, JSONL round-trip, sink-
  path resolution (explicit env vs. rules-dir derivation).
- Fleet: `emit_stage_reports` maps clean / bounced-resolved / bounced-residual stages to
  the right `ReviseIteration` + `Layer2Result` + `StageFinished`; new `BuildEvent`
  variants are Clone+Debug.
- Server: `parse_gate_line` (blank/malformed skipped), `gate_record_to_event` (gate
  allow/deny → layer-1, delegate dispatch/return, legacy no-`kind` default),
  `build_event_to_gate_event` (each variant's layer/verdict; `Verifying` → None).
- UI: `RunGateEvent` parses `layer` and defaults when absent; `live_event_style` labels
  each layer/verdict distinctly.

`cargo build --workspace -j2` + `cargo test --workspace` green.
