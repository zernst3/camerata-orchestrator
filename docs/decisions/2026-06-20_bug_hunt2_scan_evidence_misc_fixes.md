# Bug Hunt 2 — Scan / Evidence / Misc Fixes

**Date:** 2026-06-20
**Branch:** fix2/scan-evidence-misc
**Source audit:** `docs/audits/2026-06-20_waves45_bug_hunt.md`
**Files changed:**
- `crates/server/src/ai_audit.rs`
- `crates/server/src/evidence.rs`
- `crates/persistence/src/artifacts.rs`
- `crates/ui/src/cockpit.rs`

---

## BUG-AI-1 (HIGH) — `run_passes_batch` ignores `advisory_disabled`

**Root cause:** The parallel path (`run_passes`) accepted an `advisory_disabled` flag so
language-scoped routing groups could suppress the novel-issue discovery pass and avoid
duplicate advisory findings per file. The batch path (`run_passes_batch`) had no such
parameter and unconditionally set `advisory = bi == 0` for every (chunk, batch) pair,
meaning each language group's first batch re-ran the advisory pass.

**Fix:** Added `advisory_disabled: bool` parameter to `run_passes_batch`. The advisory
guard is now `!advisory_disabled && bi == 0`, mirroring `run_passes`. The call site in
`audit_repo` passes `false` (advisory enabled) since batch mode does not yet apply per-rule
routing and treats all rules as a single Scope::All group.

**Regression test:** `bug_ai1_advisory_disabled_suppresses_advisory_in_batch_mode`

---

## BUG-AI-2 (MED) — `canonical_adopted_rule` PANIC false-positives

**Root cause:** The match `has("PANIC")` was too broad: any invented rule name containing
the substring "PANIC" (e.g. `"PREVENT-PANICKING-AUTH-CHECK"`, `"LOG-PANIC-RECOVERY-1"`)
was canonicalized to `ARCH-STRUCTURED-ERRORS-1`, even when that rule is about structured
error types, not panic handling.

**Fix:** Narrowed the condition to require at least one of the model's repeatedly-emitted
panic-at-callsite co-terms: `HANDLER`, `UNWRAP`, `UNHANDLED`, `ON-ERROR`, `PROPAGAT`,
`UNWIND`, `BUBBL`. Rules that merely mention "PANIC" without any of these stay as
AI-prefixed novel findings.

**Regression tests:**
- `bug_ai2_canonical_adopted_rule_panic_match_is_narrow` — non-panic PANIC rules are not canonicalized
- `bug_ai2_canonical_adopted_rule_panic_not_adopted_returns_none` — not-adopted guard holds

---

## BUG-AI-3 (LOW) — calibration-model fallback comment was misleading

**Root cause:** The comment said "fall back to the SCAN model" but when `audit_model` is
`None` (no explicit model, no env var), the fallback is `None` (LLM default), not a named
scan model. The code was correct; the comment was inaccurate.

**Fix:** Clarified the comment to accurately describe the three-level chain and the
None-when-all-absent case. Added a test documenting the expected None behavior.

**Regression test:** `bug_ai3_calib_model_none_when_all_sources_absent`

---

## BUG-EV-1 (MED) — `canonical_json_for_hashing` silently hashes empty string on failure

**Root cause:** `serde_json::to_string(&tmp).unwrap_or_default()` — a serialization failure
(possible with non-finite floats or future custom Serialize impls) silently produced `""`,
and `fnv1a_hex("")` is a valid-looking deterministic hash. `verify_hash` would then return
`true` for any other record that also failed to serialize, producing a false-positive
tamper-check and undermining the SOC-2 audit-trail integrity guarantee.

**Fix:**
- `canonical_json_for_hashing` now returns `Result<String, serde_json::Error>`.
- `compute_hash` matches on the result: on success, sets the FNV hash; on error, logs via
  `eprintln!` and sets `content_hash = "HASH-ERROR"` (a sentinel that `verify_hash`
  explicitly rejects).
- `verify_hash` returns `false` for empty `content_hash` OR `"HASH-ERROR"`.

**Regression tests:**
- `bug_ev1_compute_hash_produces_non_empty_hash_for_normal_record`
- `bug_ev1_verify_hash_round_trip`
- `bug_ev1_verify_hash_returns_false_for_hash_error_sentinel`
- `bug_ev1_tamper_detection_still_works`

---

## BUG-EV-2 (LOW) — `render_pr_markdown` drops `ChangeLink.label`

**Root cause:** The `if !link.label.is_empty()` block in the Change Links section contained
only a developer comment and no code. Every `ChangeLink` with a non-empty `label` (e.g.
a PR title or commit message) was silently dropped from the PR markdown output.

**Fix:** Added `out.push_str(&format!("  _{}_\n", escape_md_table(&link.label)));` inside
the if-block so labels are rendered as an italicized sub-bullet under the bare ref line.

**Regression tests:**
- `bug_ev2_render_pr_markdown_includes_change_link_label`
- `bug_ev2_render_pr_markdown_no_label_renders_kind_and_ref`

---

## BUG-AS-1 (LOW) — `row_to_revision` silently substitutes `Utc::now()` for corrupt timestamps

**Root cause:** `created_at_str.parse().unwrap_or_else(|_| Utc::now())` — a row with an
unparseable `created_at` column (corrupt DB, format mismatch, manual edit) would return an
`ArtifactRevision` where `created_at` is the retrieval time, not the storage time. For an
append-only audit-trail store, a timestamp substitution is an integrity violation: the
revision history is no longer tamper-evident and the error is invisible to callers.

**Fix:** Replaced `unwrap_or_else` with `.map_err(|e| sqlx::Error::Decode(...))` — the
same error-propagation pattern used for `kind`, `actor`, and `op` parse failures in the
same function. Callers receive a `PersistenceError` and can decide whether to surface or
skip the row.

**Regression test:** `bug_as1_corrupt_timestamp_propagates_error_not_utc_now` — injects a
row with `created_at = 'NOT-A-TIMESTAMP'` directly into the in-memory DB and verifies both
`current_artifact` and `history` return `Err`.

---

## BUG-UI-1 (MED) — UI fast-model default mismatches fleet canonical id

**Root cause:** `cockpit.rs::default_fast_model_str()` returned `"claude-haiku-4-5"` while
`tier.rs::default_fast_model()` returned `"claude-haiku-4-5-20251001"`. Opening the model
tier settings panel on a project with no explicit fast-tier setting would show the wrong
id, and hitting Save without changes would pin `"claude-haiku-4-5"` to the project
config — a different model id than the fleet would have used.

**Fix:** Updated `default_fast_model_str` in `cockpit.rs` to `"claude-haiku-4-5-20251001"`
and the placeholder text at the fast-tier input to match. No fleet-side change needed.

**Behavioral test:** The existing `tier_map_defaults_when_absent_from_legacy_project_json`
test in `model_tier.rs` already validates `"claude-haiku-4-5-20251001"` at the fleet
layer; the UI constant alignment is verified by inspection (no runtime test is feasible
for a compile-time constant in a UI component).

---

## Not fixed in this branch (out of scope)

- BUG-UOW-1/2/3/4 (`uow.rs`) — explicitly out of scope per the HARD CONSTRAINTS:
  `uow.rs` is not in the allowed file list and must not be touched.
