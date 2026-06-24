# Onboarding state fixes: rule selection auto-save + re-audit stale state

Date: 2026-06-23
Branch: fix/onboarding-state-bugs
PRs: n/a (bug fixes committed directly)

---

## Bug 1: Rule selection not auto-saved across page reloads

### Root cause

`ProposedRulesTable` runs two one-shot hooks at mount:

1. `use_hook` computes `suggested_ids` by reading `repo_selection.peek()` synchronously
   (line ~2461 in rules.rs before the fix). At mount time `repo_selection` is empty or
   contains only the recommended pre-seed — the async `use_future` that restores the draft
   (`load_onboarding_draft`) has not run yet.

2. A second `use_hook` (line ~2513) pre-selects the table checkboxes from `suggested_ids`.
   Because step 1 read an empty map, only auto-recommended rules are checked.

3. The writeback `use_effect` (line ~2534) fires immediately after mount because
   `handle.selected_ids()` is subscribed. It writes the recommended-only checkbox state
   BACK into `repo_selection`, overwriting it.

4. When the async draft restore later calls `repo_selection_w.set(d.repo_selection)` it
   sets the correct saved selection — but the writeback effect immediately fires again
   (selection didn't change from the table's perspective) and overwrites it again with the
   recommended-only set.

The net effect: the draft is restored into `repo_selection` and immediately clobbered;
the architect's saved rule picks are lost on every reload.

### Fix

Two-pronged approach:

**Guard the writeback** (`rules.rs` writeback `use_effect`):

```rust
if !draft_loaded() {
    return;
}
```

`draft_loaded` is a `Signal<bool>` created in `ScanResults` immediately after
`use_signal(|| false)` and provided as context via `use_context_provider(|| draft_loaded)`.
`ProposedRulesTable` consumes it with `use_context::<Signal<bool>>()`.

This prevents the writeback from overwriting `repo_selection` during the window between
mount and async draft restore.

**One-shot restore effect** (`rules.rs` after writeback effect):

A second `use_effect` watches `draft_loaded` and `repo_selection`. When `draft_loaded`
first becomes `true`, it:
1. Reads the saved ids from `repo_selection` under `selection_key(&view_repo)`.
2. Clears all current table checkboxes.
3. Re-checks exactly the saved ids.
4. Sets `selection_restored = true` (a local `Signal<bool>`) to prevent re-running on
   subsequent `repo_selection` writes, which would fight the user's live ticks.

This re-syncs the visual checkbox state after the async restore, since `use_hook`'s
pre-select ran before the draft was available.

---

## Bug 2: Re-audit shows stale state from the prior run

### Root cause

The `on_audit` handler in `ScanResults` (scan.rs) cleared `audit`, `job_progress`,
`det_progress`, and set `auditing = true`. It did NOT clear:

- `dispositions`: per-finding triage state (Resolved, TechDebt, Ignored, etc.) from
  the prior run. These are keyed by finding id, and finding ids can collide across runs
  (same file + rule), so old triage state would appear attached to new findings.
- `triage_view`: the active filter tab (Unresolved / Resolved / Ignored / TechDebt).
  If the architect was on "Ignored" when they triggered re-audit, new findings would
  load into Unresolved but the filter would stay on "Ignored", showing an empty table.
- `detail_finding`: the currently-open finding detail modal. After re-audit the old
  finding no longer exists in the new results, so the modal would show stale data.

### Fix

In `on_audit` handler (scan.rs), after `audit.set(None)`:

```rust
dispositions.set(std::collections::HashMap::new());
triage_view.set(TriageState::Unresolved);
detail_finding.set(None);
```

All three are Dioxus `Signal<T>` (Copy), captured by value in the `move` closure.

---

## Signals: reset vs persist on re-audit

| Signal | Reset on re-audit? | Rationale |
|---|---|---|
| `audit` | YES | Prior findings are for the prior run |
| `dispositions` | YES | Per-finding; finding ids may collide; stale triage is confusing |
| `triage_view` | YES | Always start fresh on Unresolved |
| `detail_finding` | YES | Prior finding modal makes no sense for new run |
| `job_progress` | YES | Progress bar for the prior job |
| `det_progress` | YES | Deterministic scan progress for the prior run |
| `repo_selection` | NO | Architect's rule choices are intentional and persist |
| `chosen` | NO | Per-rule alternative picks persist |
| `custom_rules` | NO | User-authored custom rules persist |
| `auditing` | set to true | Marks the new run as in-progress |

---

## Files changed

- `crates/ui/src/cockpit/scan.rs`
  - Added `use_context_provider(|| draft_loaded)` after `draft_loaded` is created
  - Added reset of `dispositions`, `triage_view`, `detail_finding` in `on_audit` handler
  - Added `#[cfg(test)] mod tests` with `onboarding_draft_back_compat_missing_optional_fields`

- `crates/ui/src/cockpit/rules.rs`
  - Added `let draft_loaded = use_context::<Signal<bool>>()` in `ProposedRulesTable`
  - Added `if !draft_loaded() { return; }` guard at top of writeback `use_effect`
  - Added one-shot restore `use_effect` after writeback effect
  - Added `#[cfg(test)] mod tests` with `selection_key_*` tests
