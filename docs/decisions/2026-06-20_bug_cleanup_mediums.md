# Bug Cleanup — Medium + Low Findings (Wave 1-3 Merged Codebase)

**Date:** 2026-06-20
**Branch:** dev6/bug-cleanup
**Agent:** dev6/bug-cleanup subagent (Claude Sonnet 4.6)
**Scope:** Fixes for BUG-2, BUG-9, BUG-11, BUG-6 (MEDIUM) and BUG-10, BUG-12, BUG-7, BUG-8 (LOW)
from the `docs/audits/2026-06-20_integration_bug_hunt.md` report.
**Skipped:** INT-1 (Finding.provenance schema change in onboard.rs — out of confine; ROUTE to Zach)

---

## BUG-2 (MED) — PrCoverage omitted from UI ProcessRuleConfigView

**File:** `crates/ui/src/vcs_settings.rs`

**Root cause:** `ProcessRuleConfigView` did not include a `pr` field mirroring
`ProcessRuleConfig.pr: PrCoverage`. When the UI POSTed config, the server deserialized
the missing `pr` block as `PrCoverage::default()` (`apply_body_rule = true`,
`apply_id_rule = true`), silently overwriting any custom value the project had set.

**Fix:**
- Added `PrCoverageView` struct with `apply_body_rule: bool` and `apply_id_rule: bool`,
  both defaulting to `true` via `#[serde(default = "yes")]`.
- Added `pr: PrCoverageView` field to `ProcessRuleConfigView` with `#[serde(default)]`.
- Added a "PR coverage" section to the settings form with two checkboxes: one for
  `apply_body_rule` (`PROCESS-COMMIT-DOC-1` on PR body) and one for `apply_id_rule`
  (`PROCESS-ADO-LINK-1` on PR title).

**Tests:** Three tests in `vcs_settings::tests` (BUG-2 regression suite):
- `bug2_pr_coverage_round_trips_through_serde` — serialized config includes the `pr`
  field and a non-default `apply_body_rule = false` survives the round-trip.
- `bug2_pr_coverage_defaults_to_true_true` — empty JSON object deserializes with both
  flags `true`, matching the server default.
- `bug2_pr_coverage_is_top_level_field_on_config` — `pr` is a top-level JSON object
  with the two boolean sub-fields.

---

## BUG-9 (MED) — Sign-off not persisted into UoW's durable evidence record

**File:** `crates/server/src/uow.rs` (UowStore::sign_off)

**Root cause:** `UowStore::sign_off` set `uow.sign_off` but did NOT update
`uow.evidence` with the sign-off. Only the PR-comment clone (`evidence_for_pr` in
`lib.rs`) received the sign-off. A QA reviewer reading `uow.evidence` directly (via
the cockpit or any downstream verifier) saw evidence with `sign_off: None` and the
pre-sign-off hash, even after the architect had signed off.

**Fix:** Inside the mutex block in `UowStore::sign_off`, after setting `uow.sign_off`,
call `ev.set_sign_off(&sign_off); ev.compute_hash()` on `uow.evidence.as_mut()` when
the evidence record is present. The `sign_off` local was changed from a `move` capture
to `.clone()` on the first use so the value remains available for the evidence update.

The PR-comment path in `lib.rs` reads `uow.sign_off` (already set) and calls
`set_sign_off` again on its clone — this is idempotent and harmless.

**Tests:** Three tests in `uow::tests` (BUG-9 + BUG-10 + BUG-12 regression suite):
- `bug9_sign_off_is_reflected_in_durable_evidence_record` — attaches evidence, signs
  off, reads back `uow.evidence`, asserts `sign_off` is present and `verify_hash()`
  returns `true` for the signed state.
- `bug9_sign_off_without_evidence_still_works` — verifies no panic when no evidence
  is attached at sign-off time.

---

## BUG-11 (MED) — ScanCacheStore::put releases mutex before save (TOCTOU)

**File:** `crates/server/src/scan_cache.rs`

**Root cause:** `put` acquired the mutex, inserted the manifest, released the mutex
(closing the `if let Ok(mut s) = self.inner.lock()` block), then called `self.save()`.
`save()` re-acquired the mutex independently. Between the two lock operations, a
concurrent `put` from another project's scan could acquire the lock, insert its own
manifest, and call `save()`. If the second `save()` finished before the first, the
first `save()` would write the combined (correct) state — but if the first `save()`
finished first, it would write only the first project's state and the second `save()`
would overwrite with both. Under parallel scanning, the final file state could drop
the second project's manifest.

**Fix:** Extracted `save_locked(&CacheState)` that accepts a reference to the already-
locked state and writes to disk. `put` and `clear` now call `save_locked` while still
holding the `MutexGuard`, eliminating the window. The old `save()` method is kept
`#[allow(dead_code)]` for potential test use.

**Tests:** Two tests in `scan_cache::tests` (BUG-11 regression suite):
- `bug11_put_is_atomic_in_memory` — two sequential `put` calls must both be visible
  after completion; `clear` of one must not affect the other.
- `bug11_put_after_clear_restores_cleanly` — `put` after `clear` stores the new
  manifest, not the cleared-era state.

---

## BUG-6 (MED) — ScanMode::Batch uses RULE_BATCH_SIZE (15) from parallel mode

**File:** `crates/server/src/ai_audit.rs` (ScanMode::tuning)

**Root cause:** `ScanMode::Batch` shared `(PARALLEL_CONCURRENCY, RULE_BATCH_SIZE)`
with `ScanMode::Parallel` from `tuning()`. In Parallel mode, a small `RULE_BATCH_SIZE`
(15) is correct: more batches = more concurrent API calls = lower wall-clock time.
In Batch mode, all (chunk × rule-batch) items are compiled into one Anthropic Message
Batch and submitted together — there is no per-item concurrency cost. Splitting into
15-rule batches fragmented the batch unnecessarily; a larger batch keeps all adopted
rules visible in one prompt context, improving coherence and reducing cross-batch
re-flagging under different invented rule names.

**Fix:**
- Added `BATCH_RULE_BATCH_SIZE = usize::MAX` module-level constant (all rules per
  chunk, one BatchItem per chunk).
- Added `CAMERATA_BATCH_RULE_BATCH_SIZE` env-var override so operators can cap the
  batch rule size for very large rule sets.
- `ScanMode::Batch` arm in `tuning()` now returns `BATCH_RULE_BATCH_SIZE` (or the
  env-var value) as the `batch_size`, and keeps `PARALLEL_CONCURRENCY` for the
  `concurrency` return (unused by the batch path, but structurally consistent).

**Tests:**
- `bug6_batch_mode_uses_larger_rule_batch_size_than_parallel` — asserts Batch's
  `batch_size > Parallel's `batch_size` (fails before fix, passes after).
- `bug6_sequential_mode_tuning_unchanged` — asserts Sequential still returns
  `(1, usize::MAX)` (no regression).

---

## BUG-10 (LOW) — GateProvenance.total_bounces and deny_count are separable fields

**File:** `crates/server/src/uow.rs`

**Root cause:** `GateProvenance` had both `deny_count` and `total_bounces` as
independently settable fields, documented as always equal. Any code path that set them
to different values produced an inconsistent record with no runtime enforcement.

**Fix:**
- Updated the `GateProvenance` rustdoc to explicitly state the invariant.
- Added `GateProvenance::new()` canonical constructor that takes `deny_count` and
  derives `total_bounces = deny_count` automatically.
- Added `debug_assert_eq!(provenance.total_bounces, provenance.deny_count)` in
  `UowStore::record_gate_provenance` to catch callers that still use struct literals
  with mismatched values (fires only in debug/test builds — non-fatal in production).

**Tests:** `bug10_gate_provenance_new_enforces_invariant` in `uow::tests`.

---

## BUG-12 (LOW) — sign_off_run block check is non-atomic with sign_off mutation

**File:** `crates/server/src/uow.rs` (partial mitigation)

**Root cause:** The `sign_off_run` handler in `lib.rs` (out of confine) calls
`state.uow.get_or_create` to snapshot the UoW, checks `snapshot.is_sign_off_blocked()`,
then calls `state.uow.sign_off`. Between the snapshot and the mutation, a concurrent
`attach_evidence` could change the block state.

**Partial fix:** Added `UowStore::is_sign_off_blocked(story_id: &str) -> bool` that
reads from the live in-memory map under the mutex (not from a stale `get_or_create`
snapshot). The handler in `lib.rs` can switch to this method to reduce the TOCTOU
window from "snapshot age" to "method-to-sign_off gap" (a much narrower window under
the single-server architecture). A full fix (block check inside the sign_off mutex)
requires touching `lib.rs` and is out of this agent's confine.

**Tests:** `bug12_store_is_sign_off_blocked_reads_live_state` in `uow::tests` —
verifies the method correctly reflects the current evidence state across absent /
non-critical / critical transitions.

---

## BUG-7 (LOW) — merge_location_group tiebreaker idiom clarity

**File:** `crates/server/src/ai_audit.rs`

**Root cause:** `max_by_key(|(i, f)| { ..., group.len() - i })` prefers earlier
findings (lower `i`) because `group.len() - i` is LARGER for smaller `i`. The comment
said "earliest appearance" but the idiom looks like it favors larger indices. The audit
(BUG-7) correctly noted no correctness bug — only a readability issue.

**Fix:** Added explicit comment explaining the `group.len() - i` tiebreaker idiom and
noting the equivalent `min_by_key(|(i, _)| i)` alternative. No behavior change.

**Tests:** `bug7_merge_location_group_earliest_appearance_wins_on_tie` — three AI-
findings with equal priority; verifies index-0 is selected as primary and the others
appear in `also_matches` in order.

---

## BUG-8 (LOW) — build_digest truncation notice emitted even for trivial truncations

**File:** `crates/server/src/ai_audit.rs`

**Root cause:** The truncation notice `[digest truncated …]` was appended whenever ANY
content was cut, even if only a single closing brace was dropped. This could mislead
the model into thinking significant context was missing when effectively all code was
present.

**Fix:**
- Added `TRUNCATION_NOTICE_MIN_DROP: usize = 400` pub(crate) constant (5 lines × 80
  chars — the minimum meaningful drop).
- `build_digest` now only appends the notice when `dropped >= TRUNCATION_NOTICE_MIN_DROP`.
  When no partial slice was added (whole file dropped), `significant_truncation = true`
  unconditionally.

**Tests:**
- `bug8_trivial_truncation_does_not_emit_notice` — builds a two-file digest where the
  second file's truncation drops fewer than `TRUNCATION_NOTICE_MIN_DROP` chars; verifies
  no notice.
- `bug8_significant_truncation_emits_notice` — a large second file (most content
  dropped) still emits the notice.

---

## Routing note: INT-1 (SKIP)

INT-1 recommends adding a `provenance: Option<String>` field to `Finding` in
`crates/server/src/onboard.rs` to distinguish deep-tier advisory findings from
deterministic floor findings. This touches `onboard.rs` which is out of this agent's
confine. Routed to Zach for manual decision.

---

## Files changed

- `crates/ui/src/vcs_settings.rs` — BUG-2: PrCoverageView + form section + tests
- `crates/server/src/uow.rs` — BUG-9, BUG-10, BUG-12: sign_off evidence update,
  GateProvenance::new constructor + debug_assert, UowStore::is_sign_off_blocked + tests
- `crates/server/src/scan_cache.rs` — BUG-11: save_locked atomicity + tests
- `crates/server/src/ai_audit.rs` — BUG-6, BUG-7, BUG-8: tuning fix, comment clarity,
  truncation threshold + tests
- `docs/decisions/2026-06-20_bug_cleanup_mediums.md` — this file
