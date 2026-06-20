# Bug Hunt Fixes — Wave 5 (BUG-1, BUG-3, BUG-4, BUG-5)

**Date:** 2026-06-20
**Branch:** dev5/bug-fixes
**Source audit:** `docs/audits/2026-06-20_integration_bug_hunt.md`
**Files touched:** `crates/checks/src/vcs_action.rs`, `crates/server/src/ai_audit.rs`

## Summary

Fixes three HIGH and one MEDIUM finding from the wave 1-3 integration bug hunt.
All fixes are in the two files this agent is authorised to touch. Findings in
other files are listed under "Routed / Out-of-scope" at the bottom.

---

## BUG-5 (HIGH) — `consensus_verdicts` tie-breaks toward "high", contradicting spec

**File:** `crates/server/src/ai_audit.rs`
**Root cause:** The severity-resolution branch checked `counts[2] == max` (high)
first, so any tie where high and medium (or all three) had equal vote counts
resolved to "high". The comment at the function head explicitly says "ties break
to the LOWER severity" (humble/conservative design). The code was the OPPOSITE of
the documented spec.

**Fix:** Reversed the preference order to check low first:

```rust
// Before (wrong — tied to "high"):
let sev = if counts[2] == max { "high" } else if counts[1] == max { "medium" } else { "low" };

// After (correct — tied to "low"):
let sev = if counts[0] == max { "low" } else if counts[1] == max { "medium" } else { "high" };
```

**Tests added:**
- `bug5_consensus_tie_breaks_to_lower_severity_not_higher` — high(1) vs medium(1) tie must resolve to medium.
- `bug5_three_way_tie_resolves_to_low` — 1-1-1 three-way tie must resolve to low.
- `bug5_unanimous_high_stays_high` — non-tie regression guard.

---

## BUG-4 (HIGH) — Resolution round `add_total` inflates progress bar in Batch mode

**File:** `crates/server/src/ai_audit.rs`
**Root cause:** In `audit_repo`, after the main batch scan completes,
`run_passes_batch` already called `jstore.add_total(jid, total)` with the full
batch item count. The resolution round (lines ~1813-1814) then unconditionally
called `jstore.add_total(jid, res_chunks.len() * batches_res.len())` again.
Because `add_total` accumulates, this pushed the denominator up AFTER the batch
had already brought `done == total`, causing the UI progress bar to drop from
100% back to a lower value.

**Fix:** Guard the resolution round's `add_total` call so it only runs when
`mode != ScanMode::Batch`:

```rust
if mode != ScanMode::Batch {
    if let Some((jstore, jid)) = job {
        jstore.add_total(jid, res_chunks.len() * batches_res.len());
    }
}
```

In Batch mode the resolution items still call `inc_done` (from `run_passes`),
which pushes `done` slightly past `total`. This is invisible to the UI (the
progress bar clamps at 100%) and far less disruptive than the denominator-inflate
glitch. In non-Batch modes the resolution items correctly extend the denominator
(the main passes pre-seeded it, not the resolution items).

**Tests added:**
- `bug4_batch_mode_resolution_add_total_is_guarded` — verifies the guard logic
  is correct for `ScanMode` variants (unit-level guard; full async path would
  require a real LLM for an integration test).

---

## BUG-1 (HIGH) — `IdLocation` field stored but never consulted in `build_rules`

**File:** `crates/checks/src/vcs_action.rs`
**Root cause:** `CommitDocConfig.id_location` (variants `Subject`, `Body`,
`Either`) was serialized, deserialized, and visible in serde round-trip tests, but
`build_rules` only ever placed the `SubstantiveWithStoryId` matcher on
`CommitBody`/`PrBody`, regardless of the field value. A project configured with
`id_location: Subject` or `id_location: Either` received silently wrong
enforcement.

**Fix:** `build_rules` now branches on `id_location` inside the
`require_story_id` block:

| `id_location` | Story-id rule targets | Substantive rule targets |
|---|---|---|
| `Body` (default) | CommitBody, PrBody | (combined in `SubstantiveWithStoryId`) |
| `Subject` | CommitSubject, PrTitle | CommitBody, PrBody (separate `Substantive` rule) |
| `Either` | CommitMessage, PrFullContent | CommitBody, PrBody (separate `Substantive` rule) |

A new `VcsTarget::PrFullContent` variant was added (title + `"\n"` + body
concatenated) to support the `Either` case on PR actions without redesigning the
`evaluate` function. `VcsTarget::extract` now returns
`Option<std::borrow::Cow<'a, str>>` to accommodate the owned `String` this
variant requires.

**Design decision — `Either` uses CommitMessage / PrFullContent:** Rather than
emitting two independent rules (one for subject, one for body), the `Either` mode
checks the CONCATENATED full content. This means a story-id in EITHER location
passes as a single rule check, which is the correct "at least one location" semantic.
A story-id in neither location fires one violation (against the full-content target),
not two.

**Tests added:**
- `bug1_id_location_body_requires_story_id_in_body_not_subject`
- `bug1_id_location_subject_requires_story_id_in_subject`
- `bug1_id_location_either_accepts_story_id_in_subject_or_body`
- `bug1_id_location_either_pr_accepts_story_id_in_title_or_body`
- `bug1_id_location_subject_pr_checks_title_not_body`
- `pr_full_content_concatenates_title_and_body`
- `pr_full_content_returns_none_for_commit_action`

---

## BUG-3 (MEDIUM) — `contains_story_id` with `prefix="#"` + `separator='#'` builds token `"##"` and never matches

**File:** `crates/checks/src/vcs_action.rs`
**Root cause:** The function built the search token by concatenating `prefix` +
`separator`. The `empty-prefix` case was special-cased (`prefix="" → token="#"`),
but the degenerate `prefix="#" + separator='#'` case was NOT special-cased. The
resulting token `"##"` never appears in commit messages like `"Closes #42."`, so
the gate always fired even on valid commits.

The fix should have been paired with the inline doc comment at line ~166 which
suggested `prefix="#"` for a bare `#42` reference — that was the wrong spelling
(the correct canonical form is `prefix="" + separator='#'`). That misleading
comment is replaced by clearer documentation.

**Fix:** Normalise the degenerate case: if `prefix` already ends with the
separator character, do NOT append the separator again:

```rust
let token: String = if prefix.is_empty() {
    separator.to_string()
} else if prefix.ends_with(separator) {
    // Degenerate case: prefix already ends with separator; appending again
    // would produce "##" when the user meant "#". Use prefix as-is.
    prefix.to_owned()
} else {
    let mut t = prefix.to_owned();
    t.push(separator);
    t
};
```

Callers should prefer `prefix=""` (canonical); this normalisation is a defensive
fallback.

**Tests added:**
- `bug3_prefix_hash_with_separator_hash_matches_bare_hash_number`
- `bug3_story_id_in_subject_with_degenerate_prefix_passes_gate`

---

## Routed / Out-of-scope findings

These findings from the audit are in files this agent may not touch. They are
listed here so the appropriate wave-5 agents or follow-up can pick them up.

| BUG-id | Severity | File | Summary |
|--------|----------|------|---------|
| BUG-2 | MEDIUM | `crates/ui/src/vcs_settings.rs` | `PrCoverage` absent from `ProcessRuleConfigView`; silently overwritten on save |
| BUG-6 | MEDIUM | `crates/server/src/ai_audit.rs` | Batch mode ignores `concurrency`; rule-batch size tuning is wrong for Batch |
| BUG-7 | LOW | `crates/server/src/ai_audit.rs` | `merge_location_group` tiebreaker clarity (readability, not correctness) |
| BUG-8 | LOW | `crates/server/src/ai_audit.rs` | `build_digest` truncation notice fires even when near-full file fits |
| BUG-9 | MEDIUM | `crates/server/src/evidence.rs` | Sign-off not persisted to UoW's durable evidence record |
| BUG-10 | LOW | `crates/server/src/uow.rs` | `total_bounces` duplicates `deny_count` with no invariant guard |
| BUG-11 | MEDIUM | `crates/server/src/scan_cache.rs` | `ScanCacheStore::put` releases mutex before `save` |
| BUG-12 | LOW | `crates/server/src/lifecycle.rs` | `sign_off_run` block check not atomic with sign-off mutation |
| INT-1 | MEDIUM | `crates/server/src/ai_audit.rs` | Deep-tier advisory findings not distinguished by `Finding.provenance` |
| INT-2 | LOW | `crates/server/src/ai_audit.rs` | Resolution round re-runs advisory pass, potentially duplicating novel findings |

Note: BUG-6, BUG-7, BUG-8, INT-1, INT-2 are also in `ai_audit.rs` (which this
agent owns) but were not fixed in this wave to avoid scope creep — each requires
a non-trivial architectural change or refactor. They are listed here and should be
tracked in GitHub issues.
