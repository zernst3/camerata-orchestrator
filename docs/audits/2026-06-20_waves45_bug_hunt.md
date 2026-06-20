# Waves 4-5 Bug Hunt — Audit Report

**Date:** 2026-06-20
**Branch:** dev6/bug-hunt2
**Auditor:** Claude Sonnet 4.6 (Opus 4.8 Co-Author)
**Scope:** Read-only correctness audit of modules added or modified in waves 4-5.
**Files examined:**
- `crates/persistence/src/artifacts.rs`
- `crates/server/src/uow.rs`
- `crates/server/src/ai_audit.rs`
- `crates/server/src/evidence.rs`
- `crates/fleet/src/tier.rs`
- `crates/server/src/model_tier.rs`
- `crates/agent/src/post_story_hook.rs`
- `crates/ui/src/cockpit.rs`
- `crates/ui/src/vcs_settings.rs`
- `crates/rules/principles/go/` (all TOML files)
- `crates/rules/principles/java/` (all TOML files)
- `crates/rules/principles/csharp/` (all TOML files)

---

## Summary

| Severity | Count |
|----------|-------|
| High     | 2     |
| Medium   | 5     |
| Low      | 4     |
| **Total**| **11**|

All 11 findings are listed below, grouped by crate.

---

## crates/persistence/src/artifacts.rs

### BUG-AS-1 (Low): Timestamp parse failure silently falls back to `Utc::now()`

**File:Line:** `crates/persistence/src/artifacts.rs:405`

**Code:**
```rust
let created_at: DateTime<Utc> = created_at_str.parse().unwrap_or_else(|_| Utc::now());
```

**What is wrong:**
A `created_at` column that stored an unparseable timestamp (e.g., a corrupt row, a migration that stored a timestamp in a different format) will silently substitute the current wall-clock time. The `ArtifactRevision` returned to the caller will carry a `created_at` that is factually wrong: it is the retrieval time, not the storage time. This is an audit-trail integrity bug: the revision history is no longer tamper-evident (an attacker who corrupts the stored timestamp can shift it forward to now without any visible error).

**Why it matters:**
The artifact store is specifically designed as an audit trail for the SOC-2 evidence chain. Silently substituting `Utc::now()` for an unreadable stored timestamp undermines tamper-evidence without any log, error, or visible signal to the caller.

**Suggested fix:**
Return a `PersistenceError` on parse failure (`created_at_str.parse().map_err(PersistenceError::...)`) so the caller can decide whether to surface the error or skip the row.

---

## crates/server/src/uow.rs

### BUG-UOW-1 (High): `block_on_artifacts` catches panics to hide `current-thread` runtime incompatibility

**File:Line:** `crates/server/src/uow.rs:363-366`

**Code:**
```rust
let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
    tokio::task::block_in_place(|| handle.block_on(fut))
}));
result.ok()
```

**What is wrong:**
`tokio::task::block_in_place` panics when called from a `current-thread` runtime. The code catches that panic via `std::panic::catch_unwind`, converts it to `None`, and continues as though the artifact write simply did not happen. This means:

1. On a current-thread runtime (e.g., a single-threaded test harness, a Tokio default runtime, or `#[tokio::test]` without `flavor = "multi_thread"`), every `persist_decisions` and `set_investigation_note` call silently no-ops without any warning, log, or error.
2. The `UowStore` doc comment correctly warns "block_in_place requires the multi-thread runtime" but then relies on panic-catching as the fallback. Panics from library internals are unwind-safe only by `AssertUnwindSafe` — a soundness assertion the caller is making without evidence. If `block_in_place` panics from a genuinely unexpected state (not just wrong runtime), that internal state may be corrupted before the unwind.
3. The function returns `Option<T>` so callers cannot distinguish "no runtime attached" (expected) from "runtime panic suppressed" (unexpected). The former is fine; the latter is a silent data-loss event.

**Suggested fix:**
Use `tokio::runtime::Handle::try_current()` at call time to check the runtime is available, and attempt `block_in_place` only when `Handle::runtime_flavor()` is `MultiThread`. For `CurrentThread` runtimes, surface a `tracing::warn!` (not a panic catch) and skip the store write so the failure is observable. Alternatively, make the API async end-to-end and remove the sync bridge entirely (the correct long-term fix).

---

### BUG-UOW-2 (Medium): `decisions_for` acquires the mutex twice with a potential race between reads

**File:Line:** `crates/server/src/uow.rs:693-716`

**Code:**
```rust
pub fn decisions_for(&self, story_id: &str) -> Vec<DecisionRecord> {
    let inline = self.get_or_create(story_id).decisions;  // lock #1 (released)
    if self.artifacts.is_none() {
        return inline;
    }
    self.hydrate_inline_decisions_into_store(story_id, &inline);
    match self.load_decisions_from_store(story_id) {
        Some(from_store) => {
            if from_store != inline {
                let mut map = self.mem.lock().expect("uow mutex poisoned");  // lock #2
                if let Some(uow) = map.get_mut(story_id) {
                    uow.decisions = from_store.clone();
                }
```

**What is wrong:**
`get_or_create` acquires and releases the mutex (lock #1). Between that release and the re-lock at line 705 (lock #2), a concurrent `set_decisions` call can update `uow.decisions` in the in-memory map. The code then overwrites that concurrent update with the store's version, effectively reverting a concurrent write. This is a TOCTOU race: the "from_store != inline" check was done against the now-stale `inline` snapshot, not the current in-memory state.

**Suggested fix:**
If the in-memory sync is necessary, hold the lock across the whole read-then-write: read `inline`, compare `from_store`, and update in a single critical section. Or remove the sync-back (the comment says "keep the inline cache coherent" but the inline field is documented as a "read cache" — it does not need to be instantly in sync if the store is the authoritative source).

---

### BUG-UOW-3 (Medium): `sign_off` calls `decisions_for` which calls `block_on_artifacts` while the UoW mutex is NOT held — but `append_history` is called immediately after and re-locks

**File:Line:** `crates/server/src/uow.rs:595-636`

**Code (relevant excerpt):**
```rust
// Inside sign_off, AFTER flushing (mutex released):
let decisions = self.decisions_for(story_id);   // may do block_in_place
// ...
match hook.emit(&completion) {
    Ok(files) if !files.is_empty() => {
        // ...
        self.append_history(story_id, "story_docs", &summary);  // re-locks
        updated = self.get_or_create(story_id);  // re-locks
    }
```

**What is wrong:**
`sign_off` persists the sign-off and flushes (releasing the mutex at line 585). It then calls `decisions_for`, which may call `block_in_place` (the `block_on_artifacts` bridge). If another thread calls `set_decisions` between the flush and this point, the `decisions` captured for the hook's `StoryCompletion` will be from the store (the post-concurrent-write state) while the sign-off was recorded against the pre-update in-memory state. The `updated` UoW re-fetched at line 636 after `append_history` will see the concurrent mutation. The sign-off event and the history are coherent, but the `StoryCompletion` delivered to the hook may see a different decision set than what the gate evaluated.

This is a logic gap rather than a crash, but for an audit system the hook receives decisions that may not match what gated the sign-off.

**Suggested fix:**
Capture the decision set atomically with the sign-off: store `uow.decisions.clone()` inside the mutex-held block before flushing, and pass that frozen snapshot to the hook. Do not re-read from the store after releasing the lock.

---

### BUG-UOW-4 (Low): Hydration idempotency check calls `load_decisions_from_store` twice

**File:Line:** `crates/server/src/uow.rs:441-448`

**Code:**
```rust
fn hydrate_inline_decisions_into_store(&self, story_id: &str, inline: &[DecisionRecord]) {
    if self.artifacts.is_none() || inline.is_empty() {
        return;
    }
    if self.load_decisions_from_store(story_id).is_some() {
        return; // store already has history; nothing to migrate.
    }
    self.persist_decisions(story_id, inline);
}
```

**What is wrong:**
`hydrate_inline_decisions_into_store` is called from `decisions_for`, which also calls `load_decisions_from_store` immediately after (line 700). Each call to `load_decisions_from_store` drives an async DB round-trip through `block_on_artifacts`. The hydrate guard performs a redundant DB read: if the store has no history (the hydrate runs and writes), the subsequent `load_decisions_from_store` in `decisions_for` will return the newly-written revision. If the store already has history (hydrate exits early), `decisions_for` will re-query it. Either way the store is queried twice on the happy path of the first read for a legacy story.

**Suggested fix:**
Return a `bool` or `Option<Vec<...>>` from `hydrate_inline_decisions_into_store` indicating whether it ran, and let `decisions_for` use that result directly so only one store round-trip occurs.

---

## crates/server/src/ai_audit.rs

### BUG-AI-1 (High): `run_passes_batch` ignores `advisory_disabled` — batch mode always runs the advisory pass on every rule-batch

**File:Line:** `crates/server/src/ai_audit.rs:1176`

**Code:**
```rust
let advisory = bi == 0;
```

**What is wrong:**
The parallel/sequential path (`run_passes`) receives an `advisory_disabled` parameter that suppresses the "flag novel issues beyond the adopted rules" task when running language-scoped groups. The batch path (`run_passes_batch`) does not have this parameter and unconditionally sets `advisory = bi == 0` for every chunk/batch pair. When batch mode is used with routed (language-scoped) rules, the advisory pass fires in every language group's first batch, producing duplicate novel findings for every file under multiple language groups — exactly the problem the `advisory_disabled` flag was introduced to prevent in the parallel path.

The documentation at line 1664 correctly acknowledges "The Batch execution path does not yet apply per-rule routing" but does not mention the advisory duplication problem that routing was designed to fix.

**Suggested fix:**
Add an `advisory_disabled` parameter to `run_passes_batch` with the same semantics, and thread it through from `audit_repo`. Since batch mode does not yet apply per-rule routing (every rule goes against every file), the safest interim fix is to set `advisory = false` for all batches beyond `bi == 0` across all chunks and then add a comment linking to the routing follow-up. The full fix requires routing support in batch mode.

---

### BUG-AI-2 (Medium): `canonical_adopted_rule` false-positive for rules that mention "PANIC" in their directive but are not panics

**File:Line:** `crates/server/src/ai_audit.rs:247-249`

**Code:**
```rust
} else if has("PANIC") {
    "ARCH-STRUCTURED-ERRORS-1"
```

**What is wrong:**
The canonicalization function normalizes any model-invented rule name containing the substring `"PANIC"` to `ARCH-STRUCTURED-ERRORS-1`. This is too broad: a rule named `"PREVENT-PANICKING-AUTH-CHECK"` or `"LOG-PANIC-RECOVERY-1"` (both plausible invented names) would be mapped to `ARCH-STRUCTURED-ERRORS-1` even if that adopted rule is about structured error types, not panic handling. The `canonical_adopted_rule` doc says "Patterns are kept narrow to avoid mislabeling a genuinely-novel issue" but `has("PANIC")` alone is not narrow.

**Suggested fix:**
Require that the rule name specifically indicates a PANIC/UNWIND at a code point, not just any mention of "panic": e.g., `(has("PANIC") && (has("ON") || has("HANDLER") || has("UNWRAP")))`. Or, more safely, match the exact strings the model repeatedly invents (`"HANDLER-PANICS"`, `"UNHANDLED-PANIC"`, `"PANIC-ON-ERROR"`).

---

### BUG-AI-3 (Low): `verify_findings` calibration-model fallback to the SCAN model is documented but has a subtle interaction with the `calibration_model` parameter being `None`

**File:Line:** `crates/server/src/ai_audit.rs:1706-1713`

**Code:**
```rust
let calib_model = calibration_model
    .map(str::to_string)
    .or_else(|| {
        std::env::var("CAMERATA_CALIBRATION_MODEL")
            .ok()
            .filter(|s| !s.trim().is_empty())
    })
    .or_else(|| audit_model.clone());
```

**What is wrong:**
When `calibration_model` is `None`, `CAMERATA_CALIBRATION_MODEL` is unset, and `audit_model` is also `None` (which happens when both `model` parameter and `CAMERATA_AUDIT_MODEL` env var are absent), `calib_model` is `None`. `verify_findings` then passes `None` to `build_req` as `calibration_model`, which means the LLM client uses its default model. This is correct by design (per the comment "fall back to the SCAN model so the audit is end-to-end on one model by default"). However, the comment says "fall back to the SCAN model" but when `audit_model` is `None`, the fallback is `None` (the LLM's default), not a specific scan model. The comment is misleading if the intent is that calibration always runs on the same model as the scan.

**Suggested fix:**
The logic is functionally acceptable. The comment should be clarified: "fall back to the LLM's default model (same default as the scan, since `audit_model` is `None` when the default is used for both)."

---

## crates/server/src/evidence.rs

### BUG-EV-1 (Medium): `canonical_json_for_hashing` uses `unwrap_or_default` — a serialization failure silently produces an empty hash

**File:Line:** `crates/server/src/evidence.rs:423`

**Code:**
```rust
serde_json::to_string(&tmp).unwrap_or_default()
```

**What is wrong:**
If `serde_json::to_string` fails (which is rare for a well-typed struct but possible if custom `Serialize` impls are ever added, or if the struct contains a non-finite float — `f64::NAN` or `f64::INFINITY` — which `serde_json` cannot serialize), the canonical JSON falls back to an empty string `""`. `fnv1a_hex("")` is a deterministic non-empty string, so `compute_hash` will set a valid-looking but meaningless hash. `verify_hash` will then return `true` for ANY record whose fields also happen to fail serialization in the same way, producing a false-positive tamper-check.

**Suggested fix:**
Return a sentinel hash value (e.g., `"HASH-ERROR"`) on serialization failure, or propagate the error by making `compute_hash` return `Result<(), ...>`. Using `unwrap_or_default()` on a hash function is the wrong failure mode because `verify_hash` interprets the result as valid.

---

### BUG-EV-2 (Low): `render_pr_markdown` emits the label for a change link but the actual formatting logic is incomplete

**File:Line:** `crates/server/src/evidence.rs:598-605`

**Code:**
```rust
for link in &record.change_links {
    out.push_str(&format!("- {} — {}\n", link.kind, link.ref_));
    if !link.label.is_empty() {
        // Replace the last plain `ref_` with a labeled markdown link when we have a label.
        // The format above already emits the bare ref; for labeled links we'd need the URL,
        // which may BE the ref. Just append the label inline.
    }
}
```

**What is wrong:**
The `if !link.label.is_empty()` block contains only a comment and no code. The developer note says "Just append the label inline" but no label is appended. A `ChangeLink` with a non-empty `label` (e.g., `"fix: add auth check"`) will have that label silently dropped from the PR markdown output.

**Suggested fix:**
Either append the label — e.g., `out.push_str(&format!("  _{}_\n", escape_md_table(&link.label)));` — or remove the empty if-block. As-is, the `label` field on `ChangeLink` is documented ("Human label") but never rendered, which is a spec/implementation mismatch.

---

## crates/ui/src/cockpit.rs + crates/fleet/src/tier.rs

### BUG-UI-1 (Medium): UI default fast-model id `"claude-haiku-4-5"` does not match fleet default `"claude-haiku-4-5-20251001"`

**File:Line:** `crates/ui/src/cockpit.rs:204-206`

**Code:**
```rust
fn default_fast_model_str() -> String {
    "claude-haiku-4-5".to_string()
}
```

**vs. `crates/fleet/src/tier.rs:103-105`:**
```rust
pub fn default_fast_model() -> String {
    "claude-haiku-4-5-20251001".to_string()
}
```

**What is wrong:**
The UI's `TierMapView` and the fleet's `TierMap` carry different default model ids for the `fast` tier. When a user opens the model-tier settings panel on a project that has never explicitly set the `fast` tier, the UI will show `"claude-haiku-4-5"` as the default, but the fleet will resolve `"claude-haiku-4-5-20251001"`. This discrepancy means:

1. The settings panel shows a model id different from what the fleet actually uses.
2. If the user hits "Save" without changing anything, the UI will save `"claude-haiku-4-5"` to the project, which the fleet will then use — potentially resolving to a different model than the one the project previously used.
3. The `tier_map_defaults_when_absent_from_legacy_project_json` test in `model_tier.rs` validates `"claude-haiku-4-5-20251001"`, but the UI would show and save `"claude-haiku-4-5"` for the same project.

**Suggested fix:**
Align the two defaults. The fleet default (`"claude-haiku-4-5-20251001"`) is the pinned-version form and is the correct one to use for reproducibility. Update `default_fast_model_str` in `cockpit.rs` to `"claude-haiku-4-5-20251001"` and the placeholder text at line 1515 accordingly.

---

## crates/rules/principles/ (go, java, csharp corpora)

### BUG-CORP-1 (Low): All corpora load and parse correctly — no missing required fields, no duplicate ids, no malformed TOML

All 33 corpus TOML files in the three new language directories (11 Go, 11 Java, 12 C#) were verified to have:

- A non-empty `id` field matching the filename convention.
- A `title` field.
- A `[decision]` section.
- At least one `[[option]]` section.
- A `tag` field.
- A `domain` field matching the directory language.

**One structural note (not a bug, advisory):** The `csharp-sql-parameterized-1.toml` file has only 2 `[[option]]` sections while all other C# rules have 3. This is not a schema error (the TOML loader does not enforce a minimum option count), but the two-option pattern is inconsistent with the rest of the C# corpus and may appear to the UI as if an "alternative" option is missing. Worth reviewing before the C# corpus is published.

---

## Integration mismatches (cross-cutting)

### BUG-INT-1 (Medium): `block_on_artifacts` in `uow.rs` is called from sync Axum handlers — but `block_in_place` inside an Axum handler requires the `rt-multi-thread` Tokio runtime, which the server does use; however this assumption is never asserted

**File:Line:** `crates/server/src/uow.rs:346-367`

**What is wrong:**
The comment at line 356 says "the server uses rt-multi-thread" but this is a convention, not an enforced invariant. The server's Axum entry point could be changed to use `#[tokio::main]` (which defaults to `rt-multi-thread`) with fewer threads, or could be restructured in a future test harness. The `catch_unwind` fallback masks this invariant violation as a silent no-op. The intended runtime flavour should be either:

1. Checked at startup (`assert` that `Handle::runtime_flavor() == RuntimeFlavor::MultiThread`), or
2. Made unnecessary by converting the sync UoW API to async.

This is a low-priority integration mismatch today (the server does use multi-thread) but is a correctness trap for future refactors.

---

## Not-a-bug confirmations

The following suspected patterns were investigated and found to be correct by design:

- **`record_revision` transaction**: The `SELECT MAX(version) + 1` inside a transaction with the unique index backstop is correct. SQLite's `DEFERRED` transaction serializes the read-then-write at the statement level; two concurrent revisions for the same artifact will have one fail the unique index and error cleanly.
- **`back_compat_inline_decisions_hydrate_into_store` idempotency**: The `hydrate_inline_decisions_into_store` guard calls `load_decisions_from_store` and exits early if a revision already exists. The test at line 1425-1430 verifies this. Correct.
- **`decisions_for` inline cache sync**: The `from_store != inline` comparison uses `PartialEq` on `Vec<DecisionRecord>`, which requires `PartialEq` on `DecisionRecord`. Verified that `DecisionRecord` derives `PartialEq`. Correct.
- **`merge_location_group` tie-break direction**: The BUG-5 fix in wave 5 correctly inverts the tie-break to prefer the LOWER severity. The comment at line 679-681 documents this. Confirmed correct.
- **`strip_dedup_pointers` char-indexed approach**: The function uses `Vec<char>` for UTF-8 safety. The pattern matching is ASCII-only (no multi-byte pattern characters) so char-indexing is correct for this use case.
- **`scoped_audit` isolation**: The `changed_set` filter correctly uses `path.as_str()` for set membership. The function does not leak findings from non-changed files. Correct.
