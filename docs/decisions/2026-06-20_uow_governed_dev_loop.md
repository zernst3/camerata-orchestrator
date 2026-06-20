# 2026-06-20 — Wire the governed development loop over the Unit of Work (Pillar 2)

Status: implemented (additive). Author: dev1/uow-dev-loop.

## Context

Pillar 2 of the MVP is the *governed development loop*: a story must actually move
through a lifecycle, with structured data persisted at each stage, and the
**no-code-first gate** must be enforced — a governed run cannot start until the
story's decisions are approved.

Before this change the pieces existed but were not joined:

- `crates/server/src/uow.rs` (`UowStore` / `UnitOfWork`) held the dev-side projection
  of a story (branch, AI history, coarse `DevStatus`, sign-off) but had **no precise
  lifecycle stage** and no decision data.
- `crates/worktracker/src/investigation.rs` defined the pre-code gate types
  (`InvestigationArtifact`, `DecisionRecord`, `decisions_approved_for_development`) on
  `main`, but nothing in the server *called* the gate before starting a run.
- `crates/server/src/run.rs` (`execute_run`, scripted) and
  `crates/server/src/live_fleet.rs` (`execute_live_run`, gated fleet behind
  `CAMERATA_LIVE_BUILD=1`) ran the gate over tool calls, and `run_provenance` derived an
  honest accounting — but that provenance was never *persisted* onto the story.
- The cockpit stage tabs (Intake → Investigation → Plan → Status → QA) were read-only
  indicators; they did not drive or reflect a persisted lifecycle.

## What was built

All additive. No new crates, no moved module boundaries, no changed cross-crate public
trait surface (ROUTE-1 respected; see "Routed" below).

### 1. A pure lifecycle state machine — `crates/server/src/lifecycle.rs` (new module)

`UowStage`: `Intake → Investigating → DecisionsApproved → Development → AwaitingQa →
SignedOff`. Each transition is a total function on the current stage plus its
precondition, returning `Result<UowStage, TransitionError>`:

- `begin_investigation` (Intake → Investigating): always allowed from Intake.
- `approve_decisions(&[DecisionRecord])` (Investigating → DecisionsApproved): gated by
  `camerata_worktracker::investigation::decisions_approved_for_development` — at least
  one decision exists and every decision is `Approved`.
- `start_development(&[DecisionRecord])` (DecisionsApproved → Development): **re-checks**
  the decision gate (defense in depth — the gate is the product's reason to exist, so it
  is enforced again at the point of no return).
- `finish_development` (Development → AwaitingQa).
- `sign_off` (AwaitingQa → SignedOff).

`TransitionError` is a serializable enum (`WrongStage`, `DecisionsNotApproved`) with a
human-readable `message()` so the UI surfaces *exactly* what is blocking (e.g. "2 of 3
decisions still need the architect's approval"). The module is clock-free and I/O-free
(RUST-PURE-STATE-TRANSITIONS-1); 24 unit tests cover every transition, both directions,
the gate predicate wiring, wire round-trips, and the error messages.

Backward / corrective transitions (e.g. re-opening a decision sends the story back to
Investigating) are intentionally **not** modeled yet — routed as a follow-up so the
forward happy-path lands without speculative surface.

### 2. Persistence on the UoW — `crates/server/src/uow.rs` (extended)

`UnitOfWork` gains three additive, `#[serde(default)]` fields (old JSON still loads):

- `stage: UowStage` — the precise governed-development position. Defaults to `Intake`.
- `decisions: Vec<DecisionRecord>` — the story's decision records. **Why here:** the
  `ArtifactStore`-backed persistence for investigation artifacts is ROUTE-A (a public
  cross-crate API change routed to the human, still unlanded). The gate needs a durable
  place to read decision state *now*; the UoW is the natural per-story home. When ROUTE-A
  lands, this can be migrated; the gate logic reads through `UowStore` either way.
- `gate_provenance: Option<GateProvenance>` — the **frozen** copy of a completed run's
  provenance (allow/deny tallies, bounces, rules fired). `RunProvenance` is derived live
  from the in-memory `RunStore`; this freezes it onto the persisted UoW so the QA-review
  record survives the run being gone.

New `UowStore` methods own *persistence + history only*; all rule enforcement delegates
to the pure `lifecycle` functions via a private `apply_transition` helper that appends a
`stage` history entry on success and leaves the UoW untouched on failure:
`set_decisions`, `begin_investigation`, `approve_decisions`, `start_development`,
`finish_development`, `record_gate_provenance`. The existing `sign_off` now also advances
`AwaitingQa → SignedOff` (best-effort: only when the stage is legally there; never
forced). 8 new unit tests.

### 3. The gate wired into run start — `crates/server/src/lib.rs`

`start_run` now returns a `Response` and calls `ensure_development_gate` first:

- If `decisions_approved_for_development` over the UoW's decisions is false → **409
  CONFLICT** with `{ "error", "reason", "story_id" }`. No run is created. This is the
  no-code-first gate.
- If satisfied → the lifecycle stage is best-effort driven forward to `Development`
  (stepping Intake → Investigating → DecisionsApproved → Development as needed; a UoW
  already further along is left as-is, never moved backward), then the run starts.

A **provenance-stamping watcher** (`stamp_provenance_when_done`) is spawned alongside the
run executor: it polls until the run is `done` (bounded to ~5 min so a wedged live fleet
can't leak the task), then freezes the provenance onto the UoW
(`record_gate_provenance`) and advances `Development → AwaitingQa`. The executor stays
unaware of the UoW (thin layers).

New HTTP endpoints: `POST /api/uow/:story_id/decisions`,
`POST /api/uow/:story_id/begin-investigation`,
`POST /api/uow/:story_id/approve-decisions`. Lifecycle transition handlers map a
`TransitionError` to a 409 with its message via `transition_response`. 3 integration
tests assert the run is blocked until decisions are approved, proceeds (advancing the
stage) once they are, and that `approve-decisions` 409s when the gate is unsatisfied.

### 4. Cockpit — `crates/ui/src/cockpit.rs` + `crates/ui/src/style.rs`

- `UowView` gains `stage: UowStage` and `gate_provenance: Option<GateProvenanceView>`
  (UI mirrors of the server types).
- New `UowLifecycleStrip` component: a six-pip lifecycle strip (reached stages lit,
  current ringed) plus the two architect-driven forward-transition buttons —
  **Begin investigation** (enabled at Intake) and **Approve decisions** (enabled at
  Investigating). The later stages are engine-driven (the gated run + the sign-off
  action), so they are shown but not clickable here.
- A blocked transition (or a blocked run) raises a toast carrying the **server's
  reason**, so the architect sees exactly which decisions still need approval rather than
  a silent no-op. `start_run` now returns `StartRunOutcome` (`Started` / `Blocked` /
  `Failed`) to carry that reason.
- The UoW panel shows the frozen gate provenance once a run completes.
- New CSS lives inside `GLOBAL_CSS` in `style.rs` (`.uow-lifecycle`, `.uow-stage-pip`,
  `.uow-stage-btn`, `.uow-provenance`).

## How a user/dev uses it

1. Adopt a story → its UoW starts at **Intake**.
2. Click **Begin investigation** (Intake → Investigating).
3. Record the story's decisions (`POST /api/uow/:id/decisions`; the cockpit's decision
   surfaces post here). Approve them, then click **Approve decisions** — blocked with a
   precise reason until every decision is `Approved` (Investigating → DecisionsApproved).
4. Click **▶ Run this story (governed)**. The no-code-first gate runs first: blocked
   (409, toast) if decisions aren't approved; otherwise the stage advances to
   **Development** and the run starts (scripted by default; the real fleet when
   `CAMERATA_LIVE_BUILD=1`).
5. When the run finishes, the watcher freezes the gate provenance onto the UoW and the
   stage advances to **Awaiting QA**.
6. Review the provenance, then **sign off** (the explicit, never-automatic gate) →
   **Signed off**.

## How it works (data flow)

```
cockpit button ─POST─▶ /api/uow/:id/{begin-investigation,approve-decisions}
                          └▶ UowStore::<transition> ─▶ lifecycle::UowStage::<fn> (pure)
                                                          └▶ persist stage + history

cockpit "Run" ─POST─▶ /api/stories/:id/run
                        └▶ ensure_development_gate (decisions_approved_for_development)
                             ├ blocked ─▶ 409 { reason }   (no run created)
                             └ ok ─▶ drive stage→Development ─▶ start_governed_run
                                       ├ execute_run / execute_live_run (real gate)
                                       └ stamp_provenance_when_done
                                            └▶ record_gate_provenance + finish_development
```

## Routed (ROUTE-1 — not implemented here)

- **Persist investigation artifacts / decisions via `ArtifactStore`** (ROUTE-A from the
  2026-06-19 investigation doc): the cross-crate `ArtifactKind` additions. Until then,
  decisions live on the UoW (additive). Migrating them is a follow-up.
- **Backward / corrective lifecycle transitions** (e.g. re-opening a rejected decision
  returns the story to Investigating, or a failed QA returns it to Development). The
  forward path is complete; the corrective edges are a deliberate follow-up.
- **Auto-seeding decisions from a real investigation phase**: the gate reads whatever
  decisions are on the UoW; wiring the AI investigation to *produce* them is the
  investigation-phase work, separate from this gate-enforcement phase.

## Conventions honored

- RUST-PURE-STATE-TRANSITIONS-1 (pure, clock-free transitions; tests inject time).
- ORCH-NEW-PATH-TESTS-1 (comprehensive unit + integration tests for new logic).
- robustness_over_terseness (explicit field docs, verbose constructors, typed errors
  with human-readable messages, defense-in-depth re-check of the gate).
- Additive, `#[serde(default)]` on every new persisted field (old `uow.json` loads).
- New CSS inside `GLOBAL_CSS`; existing classes reused where possible.
