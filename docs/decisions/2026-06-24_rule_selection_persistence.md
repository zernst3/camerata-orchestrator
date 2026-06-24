# Rule Selection Persistence — Effect Ordering Fix

**Date:** 2026-06-24
**Branch:** fix/rule-selection-persist
**Files changed:** `crates/ui/src/cockpit/rules.rs`

## Problem

Architect rule selections (checkbox state and option choices in the onboarding
"Findings" / rule-selection UI) did not persist between app launches. Only the
suggested/recommended rules survived a relaunch; any deselections, additions of
non-recommended rules, or option choices were lost.

Commit `4b54f37` ("onboarding-state-bugs") attempted to fix this but the fix
did not hold.

## Root Cause

The persistence chain is:

```
checkbox click
  → table signal (handle.selected_ids())
  → writeback effect (rules.rs) → repo_selection signal (scan.rs)
  → auto-save effect (scan.rs) → save_onboarding_draft()
```

The restore chain on relaunch is:

```
use_future loads draft → repo_selection_w.set(saved_picks)
                       → draft_loaded.set(true)
  → restore effect (rules.rs) reads repo_selection, applies to checkboxes
  → writeback effect (rules.rs) reads corrected checkboxes, writes back to repo_selection
  → auto-save fires, persisting the correct effective set
```

`4b54f37` correctly identified that the writeback must not run before the draft
loads (it added `if !draft_loaded() { return; }`). What it missed: **Dioxus runs
effects in registration order when multiple effects are dirtied by the same signal
change.** When `draft_loaded` flips to `true`, both the writeback effect and the
restore effect are queued. Because the writeback was registered first, it ran first.

At that moment the table checkboxes still held the recommended-only seed (the
`use_hook` pre-select ran at mount, before the async draft was available). The
writeback read those stale checkboxes and overwrote `repo_selection` with the
recommended-only set — contaminating the saved draft data before the restore
effect could read it. The restore then saw `repo_selection` holding the
recommended-only seed instead of the architect's saved picks, and set
`selection_restored = true`, locking itself out of ever running again.

A secondary bug existed in the same restore effect: `selection_restored.set(true)`
was inside the `if let Some(...)` branch — so on a fresh first-view (no saved
entry), `selection_restored` was never set, permanently blocking the writeback for
every fresh-view session after `draft_loaded = true`.

## Fix

**File: `crates/ui/src/cockpit/rules.rs`**

Two changes in `ProposedRulesTable`:

### 1. Swap registration order

The restore effect is now registered **before** the writeback effect. This
guarantees that when `draft_loaded` flips to true, restore runs first (applies
saved picks to the table), then writeback runs (reads the corrected checkboxes
and propagates them to `repo_selection`).

The restore and writeback are now ordered:

```rust
// 1. RESTORE (registered first — runs first when draft_loaded fires)
use_effect(move || {
    if selection_restored() || !draft_loaded() { return; }
    // ... apply saved picks to checkboxes ...
    selection_restored.set(true); // unconditional (see fix 2)
});

// 2. WRITEBACK (registered second — runs after restore)
use_effect(move || {
    if !draft_loaded() || !selection_restored() { return; }
    // ... read corrected checkboxes, write to repo_selection ...
});
```

### 2. Gate writeback on `selection_restored`

The writeback guard changed from:

```rust
if !draft_loaded() { return; }
```

to:

```rust
if !draft_loaded() || !selection_restored() { return; }
```

This is the belt-and-suspenders guard: even if registration order somehow changed,
the writeback cannot overwrite `repo_selection` until after restore has finished.

### 3. Unconditional `selection_restored.set(true)`

The `selection_restored.set(true)` call was moved outside the `if let Some(...)`
branch. It now fires unconditionally after `draft_loaded` becomes true. This
unblocks the writeback even on a fresh first-view where there is no saved entry
for the current repo (the recommended seed is already correct in that case, so no
harm is done).

## Effect subscription reasoning

After the fix, the reactive graph is:

| Signal changed | Effects that re-run | Net outcome |
|---|---|---|
| `draft_loaded → true` | restore (first), then writeback | restore applies saved picks; writeback reads them; auto-save fires |
| user ticks checkbox | writeback (via `handle.selected_ids()`) | `repo_selection` updated; auto-save fires immediately |
| `repo_selection` | auto-save (via `.read()` in the effect body) | draft saved to BFF |
| `chosen` (option pick) | auto-save | draft saved immediately |

The `draft_loaded` gate on the auto-save effect (scan.rs line 2443) ensures no
pre-restore state is saved. The `draft_loaded || selection_restored` double-gate on
the writeback ensures the writeback never fires with stale checkboxes.

## Baseline preservation

The `suggested_ids` derivation in `ProposedRulesTable` (rules.rs ~line 2467):

- When `repo_selection` has a saved entry for this repo: restore it **exactly**.
  The effective set (recommended-that-user-kept + non-recommended-that-user-added)
  is what was saved, so restoring it preserves both the untouched suggestions and
  the user's deltas.
- When there is no saved entry (fresh first-view): fall back to
  `effective_auto_recommended()` — the recommended seed. This seed becomes the
  baseline and gets persisted as soon as the user's first checkbox change fires
  the writeback.

There is no separate delta-tracking structure: the persisted `repo_selection` IS
the effective set. Suggested rules that the user never touches stay selected
because they were in the recommended seed and the writeback always writes the
whole live selection (not just changes). User deltas win because the live
selection reflects them immediately.

## Tests added

Six new pure-logic tests across `scan.rs` and `rules.rs`:

**scan.rs (4 tests):**
- `onboarding_draft_selection_round_trips` — `repo_selection` + `chosen` survive
  serde serialization/deserialization without data loss, including the sentinel key
  for single-repo projects.
- `baseline_preservation_effective_selection_merges_correctly` — verifies that the
  restore path restores the effective set exactly (user-deselected rules stay gone,
  user-added non-recommended rules survive).
- `baseline_fallback_to_recommended_on_fresh_view` — verifies that when there is
  no saved entry, the `None` branch triggers the recommended-only fallback.

**rules.rs (3 tests):**
- `writeback_before_restore_contaminates_repo_selection` — documents the BROKEN
  pre-fix behavior: writeback before restore loses user picks.
- `restore_before_writeback_preserves_user_picks` — verifies the CORRECT ordering:
  restore first, writeback second, user picks survive.
- `fresh_view_no_saved_entry_restore_unblocks_writeback_with_recommended_seed` —
  verifies that the unconditional `selection_restored.set(true)` correctly unblocks
  the writeback on a fresh first-view with no saved entry.
