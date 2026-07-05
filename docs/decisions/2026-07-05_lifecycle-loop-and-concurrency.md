# ADR: Run-engine bounce loop, single-flight concurrency, and reject-reverts-commits

**Date:** 2026-07-05
**Status:** Accepted (Batch 3 shipped on `fix/lifecycle-loop`). LIFECYCLE-5 completed for
open-weight models on `fix/checkrunner-diagnostics`: the `CheckRunner` trait now returns
`CheckOutcome { violated, diagnostics }` and the Layer-2 bounce feeds the full toolchain diagnostics
(16 KiB cap, tail-kept) back at the prompt tail. See section 1.

## Context

The Fable 5 audit (`docs/ARCH_AUDIT_2026-07-04_fable5-complete.md`) found three more run-engine
lifecycle defects, stacked on Batch 2's cancel/provenance work (`fix/lifecycle-provenance`,
`2026-07-05_lifecycle-provenance.md`).

- **LIFECYCLE-5: the bounce loop dropped the errors (the open-weight linchpin).** The Layer-2
  bounce-and-revise loop in `crates/server/src/dev_implement_run.rs` built the agent's `task` ONCE
  and re-ran the IDENTICAL prompt on every bounce. The violated rule ids and the compiler / gate
  error output were logged as run events, then dropped. The agent got NO new information on retry, so
  a re-run was a coin flip: the whole point of a revise pass (feed the failure back so the model can
  correct) was missing.
- **LIFECYCLE-9: no single-flight guard per story.** Nothing stopped two concurrent runs from
  starting on the same story. Both resolve the SAME per-UoW worktree, so they would edit each other's
  files. Worse, a sign-off on story X could tear down the worktree out from under a still-live run on
  story X.
- **LIFECYCLE-12: reject-after-bounce left committed snapshots behind.** The bounce loop makes a
  `camerata: snapshot` COMMIT per iteration. The Reject path called `revert_worktree`, which only
  discarded UNCOMMITTED changes (`git checkout -- .` + `git clean -fd`). Every per-iteration snapshot
  commit survived the Reject and could still be pushed, breaking the UI's "revert the agent's work"
  promise.

## Decision

### 1. The bounce loop feeds the errors back at the prompt TAIL (LIFECYCLE-5)

- The implement prompt is split into a stable `base_task` (built once) and a per-pass `task`. On the
  first pass `task == base_task`. On each bounce, `task = append_bounce_feedback(&base_task,
  iteration, &feedback)` rebuilds the prompt with the failed iteration's feedback appended at the
  **tail**.
- **Tail placement is deliberate and cache-friendly.** The base prompt (story + decisions + kernel +
  grounding) is the stable, cached prefix; only the error delta at the end is new, so the KV-cache
  prefix stays warm across iterations. This is the same layering the prefix-cache prompt batch builds
  on; the append point is the seam that batch will formalize.
- **STACK-AGNOSTIC.** `feedback` is whatever the check emitted for the detected stack — the violated
  rule ids from Layer-2 (Rust clippy/test, tsc, pytest, go vet, manifest checks), the Layer-3
  reviewer's findings, or the integration-gate contract-mismatch reason. `append_bounce_feedback`
  forwards it verbatim and never names a toolchain. It is applied at ALL five bounce points: the
  Layer-2 revise, the L3 bounce (both the skip-layer2 and clean-L2 branches), and the integration-gate
  mismatch (both branches).
- The append mirrors the `directive_grounding` pattern from `resume_governed_run`: a titled block
  addressing the agent directly with the correction to apply.
- **RESOLVED (was an honest limit; landed on `fix/checkrunner-diagnostics`).** The `CheckRunner`
  trait now returns `CheckOutcome { violated: Vec<RuleId>, diagnostics: String }` instead of a bare
  `Vec<RuleId>`. `diagnostics` is the raw toolchain stdout/stderr (clippy/tsc/pytest/go vet/manifest
  output) each runner already captured and used to drop; it is now carried out of the runner. All ~8
  language/tool runner impls plus the composite (`RustCheckRunner`), polyglot, combined, and manifest
  runners populate it (empty when the tool has no meaningful stdout or the pass is clean). The Layer-2
  bounce feedback now appends the **full diagnostics** after the rule ids, at the same cache-friendly
  tail. To bound prompt size and keep the prefix cache warm, `CheckOutcome` truncates `diagnostics`
  to **`DIAGNOSTICS_CAP_BYTES` = 16 KiB**, keeping the **tail** (the failing assertion / final error
  summary is most-relevant-last) and dropping the oldest head, on a UTF-8 char boundary. A literal
  open-weight model now sees the actual error text to self-correct, not just the rule id.

### 2. Single-flight guard per story (LIFECYCLE-9)

- `RunStore::active_run_for_story(story_id)` returns the first non-`done` run on a story.
- `start_run` (the HTTP entrypoint) rejects with **409** when a story already has an active run,
  naming the active run id. The guard runs BEFORE the development gate so a duplicate start is refused
  as early as possible. The paused-then-resumed path is not blocked: a resume marks the paused run
  `done` before the resume run supersedes it (Batch 2's `mark_done(ckpt.run_id)`).
- `sign_off_run` withholds the destructive worktree teardown until `run.done`. The sign-off itself
  is still recorded; only `remove_uow_worktree` is deferred, with an honest history note, while the
  run is live. This closes the "sign-off tears down a live run" tear.

### 3. Reject hard-resets to the checkpoint base commit (LIFECYCLE-12)

- `revert_worktree` takes an optional `base_commit`. On Reject the escalation-resolve path passes the
  checkpoint's stored `base_commit`, and `revert_worktree` runs `git reset --hard <base_commit>`
  FIRST (dropping every per-iteration snapshot commit the run added since the branch point), THEN
  `git clean -fd` for untracked files.
- Without a base commit (older checkpoints), it falls back to the previous uncommitted-only revert
  (`git checkout -- .` + clean). The checkpoint already stores `base_commit`, so the fresh path always
  has it.

## Consequences

- A revise pass now actually revises: the agent sees exactly which rule / check failed AND the full
  verbatim toolchain diagnostics for Layer-2 (plus, for L3 / gate, the full reason), at a cache-warm
  tail, bounded by the 16 KiB diagnostics cap.
- Two runs can never share a worktree, and sign-off can never yank a live run's worktree.
- Reject means reject: the branch returns to its pre-run commit, with no orphan snapshot commits left
  to be pushed.

## Honest limits

- Layer-2 feedback now carries the full toolchain diagnostics (the former rule-ids-only limit is
  RESOLVED — see section 1). Remaining bound: diagnostics are truncated to 16 KiB (tail kept), so an
  error spew larger than the cap loses its oldest head lines.
- The single-flight guard reads the in-memory `RunStore`; if the process restarts mid-run the guard
  state is lost with it (consistent with Batch 2's in-memory completion signal).
