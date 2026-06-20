# Integration Bug Hunt — Wave 1-3 Merged Codebase

**Date:** 2026-06-20
**Branch:** dev4/bug-hunt
**Scope:** Waves 1-3 merged onto dev/integration. Read-only audit; no code changed.

## Summary

Modules audited:
`crates/server/src/{lifecycle.rs, uow.rs, evidence.rs, scan_cache.rs, scan_routing.rs,
ai_audit.rs}`, `crates/fleet/src/tier.rs`, `crates/checks/src/vcs_action.rs`,
`crates/ui/src/{chat.rs, vcs_settings.rs, workspace.rs}`, and the new corpus rules.

| Severity | Count |
|---|---|
| High | 3 |
| Medium | 5 |
| Low | 4 |
| **Total** | **12** |

---

## crates/checks/src/vcs_action.rs

### BUG-1 (HIGH) — `IdLocation` field is stored but never consulted in `build_rules`

**File:** `crates/checks/src/vcs_action.rs`
**Lines:** 785-816 (build_rules), 521-528 (IdLocation enum), 587 (CommitDocConfig field)

`CommitDocConfig` exposes an `id_location: IdLocation` field (variants `Subject`,
`Body`, `Either`) that is serialized, deserialized, and visible in serde round-trip
tests. `build_rules` reads `cfg.story_id_format` and `cfg.require_story_id` but
**never reads `cfg.id_location`**. Regardless of what the project configures, the
story-id is always checked in the **commit body / PR body** only (the `CommitBody` and
`PrBody` targets in `applies_to`).

Consequence: any project that sets `id_location: Subject` or `id_location: Either`
receives silently wrong enforcement. A commit with the story-id in the subject but
not the body will be rejected even though the project says "Either" is acceptable.
The bug is invisible because the field serde-rounds but has zero runtime effect.

**Suggested fix:** Inside the `cfg.require_story_id` branch of `build_rules`, apply
`cfg.id_location` to choose which targets carry the `SubstantiveWithStoryId` matcher
vs. the plain `Substantive` matcher. For `Either`, add both `CommitSubject`/`PrTitle`
and `CommitBody`/`PrBody` targets and accept if any passes; or split into two rules.

---

### BUG-2 (MEDIUM) — `PrCoverage` struct persisted and deserialized by the server but **omitted from the UI's `ProcessRuleConfigView`**

**Files:** `crates/checks/src/vcs_action.rs:733` (`ProcessRuleConfig.pr: PrCoverage`),
`crates/ui/src/vcs_settings.rs:139-149` (`ProcessRuleConfigView` struct)

`ProcessRuleConfig` has a top-level `pr: PrCoverage` field (with `apply_body_rule` and
`apply_id_rule` booleans). `build_rules` reads both fields to decide whether the PR
body and PR title are gated by `PROCESS-COMMIT-DOC-1` and `PROCESS-ADO-LINK-1`
respectively. The UI's mirror struct `ProcessRuleConfigView` does **not** include a
`pr` field at all. When the UI saves config via `POST /api/projects/:id/process-rule-config`,
the `pr` block is absent from the payload, so the server deserializes it with
`PrCoverage::default()` (`apply_body_rule = true`, `apply_id_rule = true`), silently
overwriting any custom value the user may have set. The UI also never exposes the
toggles, so a project that needs to opt out of PR gating has no way to do so.

**Suggested fix:** Add `pr: PrCoverageView` to `ProcessRuleConfigView` with matching
serde defaults; add the corresponding toggles to the settings panel.

---

### BUG-3 (MEDIUM) — `contains_story_id` with prefix `"#"` and separator `'#'` constructs token `"##"` and silently never matches

**File:** `crates/checks/src/vcs_action.rs:306-336`

The docs for `CommitDocConfig.story_id_format` say `prefix = "#"` (with separator
`'#'`) should match `#42`. However the code special-cases only the **empty-prefix**
case: if `prefix.is_empty()`, the token is just the separator `'#'`. When `prefix`
is `"#"` (not empty) and separator is `'#'`, `token` becomes `"##"`, which will never
appear in a message like `"Closes #42."` — so the gate **always fires** (the story
reference always fails), even on valid commits.

The docs for `StoryIdFormat` explicitly state `prefix="" separator='#'` means bare
`#<num>`, but a user following the `SubstantiveWithStoryId` comment at line 166
(`"#" for a bare #42`) would set `prefix="#"` and get silent breakage.

Existing tests call `contains_story_id("Closes #42.", "", '#')` (empty prefix — the
correct path) and pass. The `prefix="#"` + `separator='#'` combination is tested
nowhere, so the misalignment between the inline comment and the special-case is
undetected.

**Suggested fix:** Either remove the inline comment reference to `"#"` as a valid
prefix (it should always be `""`) and add a validation error when `prefix` ends with
the separator character; or extend the special-case to also handle the `prefix == "#"`
+ `separator == '#'` case.

---

## crates/server/src/ai_audit.rs

### BUG-4 (HIGH) — `run_passes_batch` duplicates findings via `f.clone()` and then `findings.extend(f)`

**File:** `crates/server/src/ai_audit.rs:1268-1274`

In `run_passes_batch`, for each successful batch item:

```rust
let (f, p) = parse_ai_findings(repo, &resp.text, adopted);
// ...
findings.extend(f.clone());   // extends aggregate list
// ...
jstore.add_findings(jid, f);  // sends SAME findings to job
```

`f.clone()` is fine for the job preview. However the aggregate `findings` vec receives
`f` (via `extend(f.clone())`) while the job gets `f` again — there is no double-count
in the aggregate. This is correct but compare with `run_passes` (line 1047-1050):

```rust
if let Ok((f, _, _)) = &r {
    jstore.add_findings(jid, f.clone());
}
jstore.inc_done(jid, 1);
```

In `run_passes` the job gets `f.clone()` and the aggregate gets `f` (via
`findings.extend(f)`). In `run_passes_batch` the aggregate gets `f.clone()` and the
job gets `f`. The aggregate itself is correct in both paths.

The real high-severity issue here is that **the resolution round always runs using the
parallel engine with `batches` from the outer scope**, including all rule batches:

```rust
let (rf, rp, _rn, _rok, _re) = run_passes(
    ...
    &batches,    // ← the FULL batch list, not a single advisory-only batch
    ...
)
```

The resolution round fires the advisory novel-findings pass (`bi == 0`) AND all rule
batches, which is correct. But in Batch mode, the resolution round's `run_passes` call
does not use the `job` store's `add_total` at line 1580 correctly: it calls
`jstore.add_total(jid, res_chunks.len() * batches.len())` AFTER `run_passes_batch`
already returned (which called `add_total` with the full item count). The second
`add_total` call **adds** to the total rather than replacing it, inflating the UI
progress bar denominator in Batch mode. (The `run_passes` path at line 1535 does the
same pre-seeding, so it is only a double-count when both the Batch path and resolution
round both call `add_total`.)

**Suggested fix:** For Batch mode, only call `add_total` for the resolution round if
the resolution set is non-empty (it is already conditional on `!resolution.is_empty()`)
but also guard it so it only happens when `mode != ScanMode::Batch` OR accept the
over-count is small (the resolution round is typically 1-5 files).

---

### BUG-5 (HIGH) — `consensus_verdicts` tie-breaking incorrectly prefers "high" on a tie of equal vote counts

**File:** `crates/server/src/ai_audit.rs:679-696`

```rust
let sev = if counts[2] == max {   // high wins if count == max
    "high"
} else if counts[1] == max {      // medium wins if count == max
    "medium"
} else {
    "low"
};
```

When `high` and `medium` are TIED (both have `counts[2] == counts[1] == max`), the
first branch fires and the severity is resolved to `"high"` — the more alarming
outcome. The comment at line 624 says "ties break to the LOWER severity", which is the
OPPOSITE of what the code does. With three passes and a 1-1-1 three-way tie, high also
wins (1 == 1 == 1 for all three ranks → `counts[2] == 1 == max` fires first).

The docstring is the correct specification (humble/conservative); the code is wrong.

**Suggested fix:** Reverse the preference order to "lowest severity wins on a tie":

```rust
let sev = if counts[0] == max {   // low wins first on a tie
    "low"
} else if counts[1] == max {
    "medium"
} else {
    "high"
};
```

---

### BUG-6 (MEDIUM) — `ScanMode::Batch` uses `mode.tuning()` to split rules into batches, but the batch execution path ignores `concurrency` (uses its own parallel engine)

**File:** `crates/server/src/ai_audit.rs:936-939, 1503-1508`

```rust
fn tuning(self) -> (usize, usize) {
    match self {
        ScanMode::Sequential => (1, usize::MAX),
        ScanMode::Parallel | ScanMode::Batch => (PARALLEL_CONCURRENCY, RULE_BATCH_SIZE),
    }
}
```

For `ScanMode::Batch`, `tuning()` returns `(PARALLEL_CONCURRENCY, RULE_BATCH_SIZE)`.
The caller uses `batch_size` to chunk `selected` rules into batches (line 1508). But
`run_passes_batch` does not use parallelism — it compiles ALL (chunk × rule-batch)
pairs into one Anthropic Message Batch and submits them together. The rule-batching
therefore only determines how many `BatchItem`s are created, not concurrency. There is
no per-item token cost for splitting into smaller batches, so `RULE_BATCH_SIZE = 15`
rules per batch in Batch mode may artificially fragment batches where a larger batch
would improve coherence (all adopted rules visible in one prompt context).

This is a design mismatch: the rule-batching tuning was designed for the parallel
real-time path (where each batch is a separate concurrent API call, so smaller = more
parallelism); for the Batch mode, a larger `batch_size` — or all rules in one item —
is often better.

**Suggested fix:** Either add `ScanMode::Batch => (1, usize::MAX)` (all rules per
item, one item per chunk) or document the current behaviour explicitly and add a
`CAMERATA_BATCH_RULE_BATCH_SIZE` tunable.

---

### BUG-7 (LOW) — `merge_location_group` primary selection: the tiebreaker `group.len() - i` favours EARLIEST findings but the comment says "earliest appearance"

**File:** `crates/server/src/ai_audit.rs:1318-1326`

```rust
.max_by_key(|(i, f)| {
    let adopted = u8::from(!f.rule_id.starts_with("AI-"));
    (adopted, severity_rank(&f.severity), group.len() - i)
})
```

When two findings in a location group have the same adopted-flag and severity,
`group.len() - i` is larger for **earlier** findings (lower `i`). This correctly
prefers the earliest appearance as the comment documents. No bug in behaviour, but
note that `max_by_key` with `group.len() - i` is the same as `min_by_key` with `i`.
The intent is clearer expressed as `min_by_key` reversed or with a direct comment.
Minor readability issue, not a correctness bug.

---

### BUG-8 (LOW) — `build_digest` truncation message appended even if only the LINE NUMBERS for a file that fits exactly are cut

**File:** `crates/server/src/ai_audit.rs:139-163`

When the last file's numbered content exceeds the remaining `MAX_DIGEST_CHARS` budget
by even one character, the code enters the truncation branch, adds a partial slice if
`remaining > 200`, sets `truncated = true`, and breaks. The appended message
`// [digest truncated …]` is always emitted when ANY file does not fit entirely, even
if the partial slice captured 99.9% of the file. This is not a correctness bug — it is
correct to warn — but it means a digest that captures everything except the last line
of the last file still shows the truncation notice, potentially misleading the model
into thinking significant content was dropped.

---

## crates/server/src/evidence.rs

### BUG-9 (MEDIUM) — Hash computed before sign-off is set; evidence PR comment posts a **stale hash** that cannot be verified

**File:** `crates/server/src/lib.rs:893-896` (sign_off_run handler)

```rust
let mut evidence_for_pr = evidence.clone();
if let Some(so) = &uow.sign_off {
    evidence_for_pr.set_sign_off(so);
    evidence_for_pr.compute_hash();   // ← hash recomputed after sign-off is set
}
```

This is actually handled correctly: the sign-off is set on the clone, then the hash is
recomputed. The PR comment's hash is therefore correct for the sign-off state.

However, the **UoW's persisted evidence record** (`uow.evidence`) is **not updated**
to reflect the sign-off: only the `evidence_for_pr` clone is updated. So the QA
reviewer reading `uow.evidence` directly (e.g. through the cockpit, not the PR
comment) sees the evidence with `sign_off: None` and the pre-sign-off hash. The
`verify_hash()` call on the persisted record will pass (it is consistent with itself),
but the sign-off is absent from the durable record even though it happened.

This is a state consistency bug: the durable per-UoW evidence record should be updated
when sign-off is recorded, so the cockpit and any downstream verification of the
persisted JSON sees the authoritative signed-off state.

**Suggested fix:** After `uow.sign_off(...)` mutates the UoW, also call
`uow.evidence.as_mut().map(|e| { e.set_sign_off(so); e.compute_hash(); })` and
persist with `state.uow.update` so the sign-off is durable in the evidence record.

---

### BUG-10 (LOW) — `GateProvenance.total_bounces` is documented as `== deny_count` but is a separate field that callers can set inconsistently

**File:** `crates/server/src/uow.rs:92-96`

```rust
/// Total bounces the gate sent back (== `deny_count`; named for the architect-
/// facing vocabulary).
pub total_bounces: usize,
```

`total_bounces` is said to be identical to `deny_count`, yet both are separate
persisted fields. Any caller that sets them to different values (e.g. a future code
path, a hand-edited JSON file) produces a record where `deny_count != total_bounces`,
yet there is no validation or assertion that enforces the invariant. The test at
`uow.rs:724` sets `deny_count: 2, total_bounces: 2` manually, which also has no
invariant check. The duplication of meaning adds confusion and a mutation surface.

**Suggested fix:** Remove `total_bounces` and derive it on read from `deny_count`, or
add a `debug_assert!(self.deny_count == self.total_bounces)` in the constructor.

---

## crates/server/src/scan_cache.rs

### BUG-11 (MEDIUM) — `ScanCacheStore::put` releases the mutex before calling `save`, creating a window where another `put` can interleave

**File:** `crates/server/src/scan_cache.rs:268-274`

```rust
pub fn put(&self, project_id: &str, manifest: ScanManifest) {
    if let Ok(mut s) = self.inner.lock() {
        s.by_project.insert(project_id.to_string(), manifest);
    }
    self.save();   // ← mutex released before save
}
```

Between the `}` that releases the lock and `self.save()`, another thread could call
`put` for a different project, acquire the lock, insert its manifest, and call
`save()`. Then the first thread's `save()` runs with the COMBINED state (both
manifests). This is actually correct for the file — both manifests are included. But
if the second thread's `save()` completes first and then the first thread's `save()`
completes, there is no race in the final state (last writer wins on the file, and both
hold the full map).

The actual risk is the reverse: on a slow filesystem, the second `put` inserts its
manifest, the second `save()` starts, but the first `save()` finishes first with only
the FIRST manifest. The second manifest is in memory but its `save()` writes the
combined state. Under typical single-threaded or low-concurrency use this is benign,
but under concurrent scans (Parallel mode) two projects scanning simultaneously could
see one project's manifest not persisted if the filesystem write from thread 1
completes after thread 2 already wrote the combined state. Since the cache is
best-effort this is low-impact, but the design is fragile.

**Suggested fix:** Keep the lock held through the `save()` call, or serialize writes
with a background writer channel.

---

## crates/server/src/lifecycle.rs / uow.rs — sign-off gate bypass path

### BUG-12 (LOW) — `sign_off_run` checks `is_sign_off_blocked()` against the UoW state at the START of the handler, but the UoW's evidence could be updated concurrently before `uow.sign_off()` is called

**File:** `crates/server/src/lib.rs:843-883`

```rust
let current_uow = state.uow.get_or_create(&run.story_id);
if current_uow.is_sign_off_blocked() {
    // … check waive_reason …
}
// … build note …
let mut uow = state.uow.sign_off(&run.story_id, &by, &run.id, ...);
```

`get_or_create` snapshots the UoW outside the lock. Between that snapshot and the
`state.uow.sign_off(...)` call, another request could call `attach_evidence` (e.g.
from a concurrent run finishing) and change the block state. Under the current single-
server architecture with low concurrency this is unlikely, but the check is not atomic
with the sign-off mutation. A sign-off that was legitimately blocked could be committed
in a race window.

The `UowStore.sign_off()` method holds the mutex for the duration of its mutation, but
the block check is outside that critical section. Fixing this requires either moving
the block check inside a store method that holds the lock for both the check and the
mutation, or accepting the race as acceptable under the current architecture.

---

## Cross-cutting Integration Concerns

### INT-1 (MEDIUM) — Advisory label (#62) is on `DeepLensResult.advisory` but deep-tier security findings reuse the same `Finding` struct as non-advisory floor findings; the `status` field does not distinguish them

**File:** `crates/server/src/ai_audit.rs:1808` (`security_findings: Vec<Finding>`),
`crates/onboard` (`Finding` struct)

The deep security lens populates `security_findings` with `Finding` structs whose
`status = "active"`. These are parsed via `parse_ai_findings` which sets the same
`status = "active"` as the deterministic floor findings. When the UI renders findings,
there is no structural field on `Finding` that distinguishes "deep-tier advisory AI
finding" from "deterministic floor finding" — the distinction is only accessible by
walking up to the enclosing `DeepLensResult.advisory` flag.

If findings from the deep lens are ever merged into the main findings list (e.g. by a
future aggregator), the advisory provenance would be lost. The honesty guardrail
(issue #62) requires advisory findings never be presented as if they were deterministic
floor results.

**Suggested fix:** Add a `provenance: Option<String>` or `source: &'static str` field
to `Finding` (e.g. `"floor"`, `"ai-audit"`, `"deep-ai"`) and set it in the relevant
parsers so advisory provenance travels with each finding rather than only with the
container.

---

### INT-2 (LOW) — `run_passes` advisory-pass flag (`bi == 0`) is correct in both parallel and batch paths, but is **not applied to the resolution round**

**File:** `crates/server/src/ai_audit.rs:1590-1605`

The resolution round calls `run_passes` with the same `batches` list. Since `bi == 0`
is the advisory novel-findings pass, the resolution round WILL run the advisory pass on
its chunks. This is mostly harmless (it re-runs novel-issue discovery on a small set of
files). However the comment at line 1570 says the resolution round is for "files the
model couldn't decide in a single pass" (i.e. cross-body judgment), not novel issue
discovery. Running the advisory pass again in the resolution round can introduce
duplicate novel findings that were already surfaced by the main pass, increasing
post-resolution dedup/merge work.

**Suggested fix:** Pass `batches` sliced to a single "empty advisory" batch for the
resolution round (or pass the zero-index batch only), or skip the advisory `bi == 0`
pass in resolution by passing a flag.

---

_End of report. 12 findings total (3 high, 5 medium, 4 low). No code was modified._
