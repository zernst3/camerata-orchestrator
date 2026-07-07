# ADR: Run-engine lifecycle: cancel really stops, provenance advances only on success

**Date:** 2026-07-05
**Status:** Accepted (Batch 2 shipped on `fix/lifecycle-provenance`)

## Context

The Fable 5 audit (`docs/ARCH_AUDIT_2026-07-04_fable5-complete.md`) found four run-engine lifecycle
defects. The pure `UowStage` transition table (`app-core/src/lifecycle.rs`) is clean; every bug is in
the CALLERS bypassing it.

- **LIFECYCLE-1: Cancel is a lie.** Only the investigation runner registered an abort handle and
  checked `is_cancelled`. For `dev_implement`, `pr_resolve`, `update_branch`, and the greenfield
  `live_fleet` runs, a Stop set an atomic flag no code read: the run kept going, committed, and
  pushed. Worse, the still-running executor then called `set_status(AwaitingQa, true)` and clobbered
  the Cancelled state. The Stop button both lied and shipped code.
- **LIFECYCLE-2: Provenance advanced on ANY terminal.** Every runner's failure path set
  `AwaitingQa + done` (the SUCCESS terminal). `RunStatus::Failed` had no production callers. The
  provenance watcher keyed off `run.done` alone, so a FAILED or CANCELLED run advanced
  Development → AwaitingQa and got a SOC-2 QA evidence record attached for work that never happened.
- **LIFECYCLE-3: Provenance watcher gave up after 5 minutes.** The watcher was a `MAX_POLLS = 600`
  × 500 ms poll loop. A real live run outlives it: the UoW stays stuck at Development with no
  provenance, and the sign-off gate's stage invariant breaks.
- **LIFECYCLE-4: Resume got no watcher.** `resume_governed_run` (Approve / Amend after a pause)
  spawned no provenance watcher at all, so a resumed run never got provenance, evidence, or a stage
  advance even on success.

## Decision

### 1. Cancel actually stops, and can't be clobbered (LIFECYCLE-1)

- **Every** run spawn registers a `tokio` abort handle (mirroring investigation's pattern): the two
  `live_fleet` spawns, `spawn_brownfield_dev_run` (fresh + resume), `update_branch`, `pr_resolve`.
  Aborting the task drops the agent-driver future, whose `claude` child is `kill_on_drop(true)`, so a
  Stop reaches a run blocked inside a live subprocess.
- Each runner checks `is_cancelled` between major steps and, **critically, immediately before every
  git mutation**: before commit and before push (and before the merge / merge-commit in
  `update_branch`). On cancel the runner stops BEFORE any git write: no commit, no push. A cancel
  found mid-merge in `update_branch` aborts the merge so no half-merged tree lingers.
- `RunStore::set_status` gains a **terminal guard**: it refuses to mutate a run whose `done == true`
  or whose status is `Cancelled` / `Failed`. `fail_with_reason` and `cancel` carry the same guard. A
  late executor can no longer resurrect a terminal run.

### 2. Provenance/stage advances ONLY on success (LIFECYCLE-2)

- Failure paths now call `fail_with_reason` → a genuine `Failed { reason }` terminal, not
  `AwaitingQa`. `AwaitingQa` is reserved for success. `Cancelled` stays `Cancelled`.
- `stamp_provenance_when_done` branches on the terminal STATUS:
  - **SUCCESS (`AwaitingQa`):** freeze gate provenance + `finish_development` (advance the stage) +
    assemble & attach the SOC-2 QA evidence.
  - **FAILURE (`Failed`):** still freeze gate provenance (an honest record of what the gate saw), but
    do NOT advance the stage and do NOT attach QA evidence. A failed run can never be mistaken for
    verified work at sign-off.
  - **CANCEL (`Cancelled`):** freeze nothing, advance nothing, attach nothing.
  - The enforcement-catch ledger capture runs in every terminal case (it records the gate decisions
    the run produced, independent of whether the work advanced).

### 3. Completion signal, not a 5-minute poll (LIFECYCLE-3)

- `RunStore` carries a per-run `tokio::sync::Notify` completion signal, fired by every terminal
  setter (`set_status(.., true)`, `fail_with_reason`, `cancel`, `mark_done`). `Notify` retains one
  permit, so a run that goes terminal before the watcher awaits is not lost.
- `RunStore::wait_until_done(id, safety_timeout)` awaits that signal (check-then-await to close the
  notify/observe race) and returns the terminal `Run`. The watcher awaits it instead of polling, so a
  live run of any duration is stamped the instant it finishes. The `safety_timeout` (6 h) is a
  backstop against a wedged run, never the normal path.
- The stamping is written ONCE (`stamp_provenance_when_done`) and shared.

### 4. Resume gets the watcher too (LIFECYCLE-4)

- The watcher-spawn is extracted into `spawn_provenance_watcher(state, run_id, story_id)`, called by
  BOTH `start_governed_run` (fresh) and `resume_governed_run` (resumed). A resumed run now gets the
  same completion-driven, success-gated provenance + stage advance a fresh run does.

## The success / failure / cancel provenance semantics (the table)

| Terminal status | Gate provenance frozen | Stage advanced (Dev → AwaitingQa) | QA evidence attached | Git mutations |
|---|---|---|---|---|
| `AwaitingQa` (success) | yes | yes | yes | committed + (maybe) pushed |
| `Failed { reason }` | yes (honest record) | **no** | **no** | none past the failure point |
| `Cancelled` | **no** | **no** | **no** | **none** (stopped before commit/push) |

## Consequences

- The Stop button is now honest: a cancel reaps the subprocess and guarantees no commit/push.
- The QA / sign-off gate can trust that AwaitingQa + attached evidence means the gate actually saw
  completed work; a failure is visibly `Failed` with frozen (but non-advancing) provenance.
- Live runs of arbitrary duration are stamped; the 5-minute cliff is gone.
- One shared stamping path serves fresh and resumed runs, so they cannot drift.

## Honest limits

- The completion signal is in-memory (the `RunStore` is in-memory). If the process dies mid-run the
  watcher is lost with it; the durable provenance copy remains best-effort, as before. Persisting the
  run store is out of scope here.
- The 6 h safety timeout is a coarse backstop; a run that legitimately runs longer and never signals
  would be dropped. No production path approaches it.
