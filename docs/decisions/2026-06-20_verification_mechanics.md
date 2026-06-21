# 2026-06-20 — Verification mechanics: schema extension + gates

## Status

Accepted.

## Context

`camerata-rules` ships a rule-grounding ladder
([`Verification`](../../crates/rules/src/lib.rs)): `draft` → `grounded` →
`verified`. `draft` is AI-designed and not yet checked against any external
authority; `grounded` is mapped to a cited source / real linter rule; `verified`
is a human's confirmation that the grounding is correct and is the strongest
assertion the corpus can make.

Two mechanics were missing to make the ladder operational and self-enforcing:

1. **Drift handling.** A `verified` rule is verified *against specific versions*
   of its cited sources/linters (e.g. `clippy 1.83`). When a tool version bumps,
   the human confirmation can silently go stale: the rule still claims `verified`
   but the ground moved under it.
2. **Self-promotion guard (the dogfood).** `verified` is human-only by policy,
   but nothing mechanically stopped an agent editing a rule TOML from promoting a
   rule to `verified` (or adding a `[verified]` block). Camerata governs code
   with deny-before-execute gates; its own corpus must be governed the same way.

## Decision

### 1. Fourth verification status: `NeedsRecheck`

Add `Verification::NeedsRecheck` (serde `needs_recheck`): a rule that **was**
`verified` but whose cited source/linter has since drifted. Accessor semantics:

- `is_verified()` → `true` for `Verified` only. A drifted verification is no
  longer a live human confirmation.
- `is_grounded()` / `is_shippable()` → `true` for `Grounded`, `Verified`, and
  `NeedsRecheck` (all at-least-grounded and usable); `false` for `Draft`.

`NeedsRecheck` is usable (it was grounded and still cites a source) but signals
that the human verification needs re-confirmation. The accessors now live on
`Verification` itself; `Rule` delegates, so there is one source of truth.

### 2. Verified provenance

Add `Rule.verified: Option<VerifiedProvenance>`, deserialised from a `[verified]`
TOML table:

```toml
[verified]
by = "zach"
at = "2026-06-20"
against = ["clippy 1.83", "Google Java Style Guide 2024-01"]
```

`against` records the source/linter versions verified against; it defaults to
empty and the whole table defaults to `None` when absent (additive — existing
TOMLs are unaffected). It is the durable record behind a `verified` status and
the input to the staleness pass.

### 3. Deny-gate — `verified` is human-only (the dogfood)

`camerata-checks::verification_gate::deny_verified_promotion` scans a unified-diff
changeset to the rule TOMLs under `crates/rules/principles/` and DENIES
(deny-before-execute, modelled on the existing `vcs_action` / forbidden-write
gates) any **added** line that introduces or promotes `verification = "verified"`
or adds a `[verified]` table. Message:
**`verification=verified is human-only; agents may set at most grounded`**.

Only added lines (`+`, excluding the `+++` file header) are inspected — removing
or leaving an existing `verified` line is not a promotion. Adding
`verification = "grounded"` (or any non-`verified` value), or a diff with no
verification change, passes. Exposed as both a violation list
(`deny_verified_promotion`) and a binary verdict (`gate_verified_promotion`)
reusable by the governed-dev gate.

### 4. Staleness check — drift demotes `Verified` → `NeedsRecheck`

`verification_gate::demote_if_stale(status, provenance, current_versions)` is a
pure function. For a `Verified` rule it parses each `against` entry as
`"<name> <version>"` (last whitespace token = version, so multi-word names like
`"Google Java Style Guide 2024-01"` parse correctly) and compares the version to
`current_versions[name]`. Any mismatch demotes the rule to `NeedsRecheck` and
reports the drifted entries. A name absent from `current_versions` is treated as
"unknown, not proven drifted" and does not demote on its own. Non-`Verified`
statuses pass through unchanged.

`current_versions` is a passed-in `HashMap` — a clean seam for the version input.
Live fetching of current tool versions is a deliberate follow-up; today the
caller supplies the map (a stub/param is sufficient for the deterministic core).

## Consequences

- The grounding ladder is now self-governing: an agent can ground a rule but can
  never self-promote it to `verified`, and a `verified` rule cannot silently
  outlive the versions it was checked against.
- All changes are additive and backwards-compatible: absent `[verified]` →
  `None`, absent `verification` → `Draft`, existing corpus loads unchanged.
- `camerata-checks` now depends on `camerata-rules` for the `Verification` /
  `VerifiedProvenance` types used by the staleness pass.

## Follow-ups

- Wire `gate_verified_promotion` into the governed-dev gate's
  deny-before-execute set for corpus edits.
- Live current-version fetcher feeding `demote_if_stale`'s `current_versions`
  seam (rustc/clippy `--version`, published style-guide revisions, etc.).
- A corpus-wide staleness sweep that runs `demote_if_stale` over every
  `verified` rule and writes back `needs_recheck` demotions.
