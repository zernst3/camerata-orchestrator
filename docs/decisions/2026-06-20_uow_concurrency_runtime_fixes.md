# UoW Concurrency and Runtime Fixes

**Date:** 2026-06-20
**Branch:** fix2/uow-concurrency
**Scope:** `crates/server/src/uow.rs`
**Bugs addressed:** BUG-UOW-1, BUG-INT-1, BUG-UOW-2, BUG-UOW-3, BUG-UOW-4
**Source:** `docs/audits/2026-06-20_waves45_bug_hunt.md`

---

## BUG-UOW-1 / BUG-INT-1: Explicit runtime flavour check replaces panic-catching

### Before

`block_on_artifacts` wrapped `tokio::task::block_in_place` in `std::panic::catch_unwind`
with `AssertUnwindSafe`. On a `current-thread` runtime, `block_in_place` panics; the
`catch_unwind` turned that panic into `None`, silently discarding every artifact write.
Callers could not distinguish "no runtime attached" from "runtime panic suppressed."

### After

`block_on_artifacts` calls `handle.runtime_flavor()` and branches:

- `MultiThread`: calls `block_in_place` as before (correct, server always uses this).
- Any other flavour (`CurrentThread`, future variants): emits `eprintln!` with the
  actual flavour and returns `None`, degrading to the inline/JSON path. Observable,
  not silent.

The `catch_unwind` / `AssertUnwindSafe` wrapper is removed entirely. Tests that use
`#[tokio::test]` (current-thread) now get a visible warning instead of a silent no-op.
Tests that need the store path must use `#[tokio::test(flavor = "multi_thread")]`.

---

## BUG-UOW-2: `decisions_for` collapses two lock acquisitions into one coherent pair

### Before

`decisions_for` took the inline snapshot in `get_or_create` (lock 1, released), then
re-acquired the lock (lock 2) to sync `from_store` back into the inline cache. A
concurrent `set_decisions` between those two acquisitions would be silently overwritten
by the stale `from_store` snapshot (TOCTOU race on `uow.decisions`).

### After

The inline snapshot is taken in a single `mem.lock()` scope at the top of `decisions_for`.
The store read happens outside the lock (blocking under the mutex would deadlock).
The cache-sync write re-acquires the lock and compares against the **current**
in-memory decisions, not the stale snapshot taken before the store read. A concurrent
`set_decisions` that lands between the store read and the sync-back will be correctly
preserved (its value will differ from `from_store`, so the sync write is skipped or
overwritten by the store's authoritative value, depending on ordering — either is
coherent; no stale snapshot is blindly imposed).

---

## BUG-UOW-3: `sign_off` captures decision snapshot atomically with the sign-off write

### Before

`sign_off` released the mutex (after writing the sign-off), flushed, then called
`self.decisions_for(story_id)` to get the decision set for the hook. A concurrent
`set_decisions` between the flush and the `decisions_for` call could inject a different
decision set, so the `StoryCompletion` delivered to the hook might not match the
decisions that gated the sign-off.

### After

`uow.decisions.clone()` is captured **inside** the mutex block where the sign-off is
written, before the lock is released. The hook receives this frozen snapshot regardless
of any concurrent writes that follow. The `decisions_for` call in the hook path is
removed; the frozen `decisions_snapshot` is used directly.

---

## BUG-UOW-4: `hydrate_inline_decisions_into_store` returns the store result it already loaded

### Before

`hydrate_inline_decisions_into_store` called `load_decisions_from_store` once (to check
if a revision exists), and `decisions_for` called `load_decisions_from_store` again
immediately after. Two store round-trips per `decisions_for` call on any legacy story.

### After

`hydrate_inline_decisions_into_store` returns `Option<Vec<DecisionRecord>>`:

- `None` when no artifact store is attached or inline is empty (caller proceeds normally).
- `Some(existing)` when the store already had history (returned from the idempotency check).
- `Some(inline.to_vec())` when the hydrate ran and seeded the store (the just-written set).

`decisions_for` uses this return value directly and skips the second
`load_decisions_from_store` call. One store round-trip per `decisions_for` call.

---

## Regression tests added

All four fixes have regression tests in `uow::concurrency_regression_tests`:

| Test | Bug |
|------|-----|
| `bug_uow1_current_thread_runtime_degrades_gracefully_not_silently` | BUG-UOW-1 / BUG-INT-1 |
| `bug_uow2_decisions_for_does_not_overwrite_concurrent_set_decisions` | BUG-UOW-2 |
| `bug_uow3_sign_off_hook_receives_frozen_decision_snapshot` | BUG-UOW-3 |
| `bug_uow4_hydrate_does_not_trigger_double_store_round_trip` | BUG-UOW-4 |
