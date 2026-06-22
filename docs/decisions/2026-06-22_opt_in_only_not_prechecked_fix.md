# 2026-06-22 — opt_in_only rules must never be auto-recommended or pre-checked

## Problem

`opt_in_only` rules (CICD-CODEQL-SECURITY-SCAN-1 and CICD-SEMGREP-SECURITY-SCAN-1)
were appearing in the onboarding proposed-rules table with:

1. A pre-checked checkbox (auto-selected), and
2. A "✓ Recommended" badge.

Both are wrong. `opt_in_only` rules must appear in the list so the architect can
deliberately opt in, but must never be pre-selected or badged as recommended.

## Root cause — two independent bugs

### Bug 1: UI fallback overrides the server's correct `is_auto_recommended: false`

`crates/ui/src/cockpit.rs` — `ProposedRuleView::effective_auto_recommended()`:

The server correctly sends `is_auto_recommended: false` for opt_in_only rules.
It gates on `(is_suggested || agentic) && r.is_auto_recommended() && !r.is_opt_in_only()`
in `onboard.rs`.

But the UI method, when `self.is_auto_recommended` was `false`, fell back to:

```rust
self.recommended && matches!(self.verification.as_str(), "grounded" | "verified")
```

For CodeQL/Semgrep: `recommended` was `true` (stack-relevant) and `verification`
was `"grounded"`, so the fallback returned `true` — overriding the server's deliberate
`false`. The pre-check was a UI-side re-derivation that ignored the `opt_in_only` flag
entirely.

### Bug 2: `recommended` badge did not exclude opt_in_only

`crates/server/src/onboard.rs` — the `propose_corpus_rules` mapping, field `recommended`:

```rust
// OLD — wrong
recommended: is_suggested || r.domain == "agentic",
```

This set `recommended: true` for opt_in_only rules when they were stack-relevant,
causing the "✓ Recommended" badge to appear in the UI. It also fed the UI fallback
above, compounding the pre-check bug.

## Fix

### Fix 1: `crates/ui/src/cockpit.rs` — server is authoritative

`effective_auto_recommended()` now returns `self.is_auto_recommended` directly,
with no fallback. The doc comment now states the principle:

> The server encodes the full gate (stack-relevance + grounded/verified +
> !opt_in_only) into `is_auto_recommended`. Use it directly — do NOT fall back
> to `recommended` or re-derive from `verification`.

There is no version-skew risk: the server and UI are co-versioned in this
codebase.

### Fix 2: `crates/server/src/onboard.rs` — exclude opt_in_only from `recommended`

```rust
// NEW — correct
recommended: (is_suggested || r.domain == "agentic") && !r.is_opt_in_only(),
```

opt_in_only rules now show "Available" (no badge) in the proposed-rules table,
not "✓ Recommended". This is accurate: they are present for deliberate opt-in,
not as a suggestion.

## Principle

**The server is authoritative for auto-recommend.** The server holds the full
context: stack-relevance, provenance (grounded/verified), and opt_in_only status.
The UI must honour `is_auto_recommended` as delivered, without re-deriving or
overriding it from peer fields (`recommended`, `verification`).

## Regression tests added

### Server (`crates/server/src/onboard.rs`)

Three tests in `onboard::tests`:

- `opt_in_only_grounded_stack_relevant_yields_both_false`: grounded + stack-relevant
  + opt_in_only → `recommended = false` AND `is_auto_recommended = false`. Directly
  covers the CodeQL/Semgrep case.
- `normal_grounded_stack_relevant_rule_yields_both_true`: same rule without
  opt_in_only → both `true`. Positive counterpart.
- `opt_in_only_not_stack_relevant_also_false`: also false when not stack-relevant
  (belt-and-suspenders).

### UI (`crates/ui/src/cockpit.rs`)

Six tests in the `effective_auto_recommended` block (replacing the old five):

- `effective_auto_recommended_server_true_pre_checks`: `is_auto_recommended: true`
  → pre-checked.
- `effective_auto_recommended_server_false_not_pre_checked_even_if_recommended_and_grounded`:
  **the primary regression guard** — `recommended: true`, `verification: "grounded"`,
  `is_auto_recommended: false` → NOT pre-checked. This is the exact CodeQL/Semgrep
  case. Proves the old fallback is gone.
- `effective_auto_recommended_server_false_not_pre_checked_even_if_recommended_and_verified`:
  same for `verified` provenance.
- `effective_auto_recommended_server_false_draft_not_pre_checked`: draft + server
  false → not pre-checked.
- `effective_auto_recommended_server_false_needs_recheck_not_pre_checked`: needs_recheck
  + server false → not pre-checked.
- `effective_auto_recommended_server_false_not_recommended_not_pre_checked`: not
  recommended + server false → not pre-checked.

All 568 server tests + `cargo check -p camerata-ui` green on branch
`fix/opt-in-only-not-prechecked`.
