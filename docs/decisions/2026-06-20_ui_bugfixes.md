# 2026-06-20 — Cockpit UI bug fixes (selection persistence, modal label, cost estimator, grounded badge)

Status: accepted
Scope: `crates/ui/src/cockpit.rs`, `crates/ui/src/style.rs`
Context: live bugs reported by the maintainer while testing the onboarding / audit flow.

Four independent fixes, each below with the bug, the cause, and the resolution.

## 1. Selection persistence — manual (non-recommended) picks were lost on remount

**Bug.** In the onboarding rule-selection table (`ProposedRulesTable`), rules the architect
ticked that were NOT in the recommendations were lost when they navigated away (e.g. to Governed
Development) and back. The table re-seeded to the recommended set only.

**Cause.** Per-repo selections live in a lifted `repo -> selected rule ids` map (`repo_selection`)
that is serialized into the auto-saved onboarding draft. For the MULTI-repo case each table keys
its entry by `view_repo`, so it persisted and restored correctly. For the SINGLE-repo case
`view_repo` was the empty string, and the code special-cased empty as "no map entry":

- the write-back effect only updated `selected_count` and never wrote the map, and
- the on-mount seed (`suggested_ids`) returned `None` for empty `view_repo` and always fell back
  to `recommended`.

So a single-repo scan never persisted its selection anywhere durable, and every remount reset to
recommended-only — dropping manual picks.

**Fix.** Introduce a stable sentinel map key for the single-repo case
(`SINGLE_REPO_SELECTION_KEY = "\u{0}__single_repo__"`, which can never collide with a real
`owner/repo` because those can't contain a NUL byte) and a pure helper `selection_key(view_repo)`
that returns the sentinel for empty input and the repo name otherwise. Then:

- `suggested_ids` reads the saved selection under `selection_key(&view_repo)` for BOTH cases, so
  manual picks are restored exactly (not re-derived from `recommended`).
- the write-back effect ALWAYS writes the live selection into the map under `selection_key`, so the
  single-repo selection rides the same draft-persistence path as multi-repo and survives remount.
- `CustomRulesPanel`'s auto-select writes a newly created rule under the sentinel key when there's
  only one repo, matching the key the single-repo table reads — so a new custom rule stays
  pre-checked through the table's remount.

The audit/arm request builders that branch on `view_repo.is_empty()` read the table handle's LIVE
selection (not the map), so they were already correct and are unchanged.

`selection_key` is unit-tested (sentinel for empty, passthrough for named, no collision).

## 2. Rule-detail modal — "Why the default" shown even for rules with no default

**Bug.** The rule-detail modal hardcoded the label "Why the default" whenever a decision-why
existed, even for rules with `default_option: None` (which have no default to explain).

**Fix.** Both rule-detail modals now label the section `if r.default_option.is_some() { "Why the
default" } else { "Why" }`. A rule with no default shows the rationale under a plain "Why".

## 3. Audit cost estimator — ignored scan SCOPE and the deep / SOC-2 tier

**Bug.** `estimate_audit_cost` priced only the standard scan + calibration over the whole-repo
`code_chars`. It ignored two settings the audit request actually sends:
(a) full-scan vs incremental scope, and (b) the deep / SOC-2 tier (`audit_deep`). Deep was only a
prose warning ("expect a bigger bill"), not part of the dollar figure.

**Fix.** `estimate_audit_cost` takes two new flags, `incremental` and `deep`:

- **Deep** adds three extra whole-repo passes at the AUDIT model (each reads the full code as
  input + emits a long prose report; no batch discount, no caching reuse). This is now baked into
  the returned dollars/tokens, and the readout flags deep as the MOST EXPENSIVE option (amber
  `.audit-cost-deep-note`). A unit test asserts deep > standard and deep > thorough.
- **Incremental scope** is plumbed through and surfaced in the readout ("Scope: INCREMENTAL — only
  changed files are billed, so the real cost is usually well below this whole-repo figure"). The
  client has NO changed-file token breakdown today (`ScanReportView` carries only the repo-total
  `code_chars`), so we deliberately price the FULL set and flag it as an over-estimate rather than
  guess. A unit test pins incremental == full pricing for now (see followup).

The call site passes `incremental = !audit_full_scan()` and `deep = audit_deep()`, mirroring the
actual request flags.

**Followup.** When the scan report exposes a per-file or changed-file token breakdown, price the
incremental scope over the changed-file set instead of the whole repo (and drop the
`incremental == full` test's equality assumption).

## 4. Grounded verification tier — faint, no distinct symbol on the rule tables

**Bug.** On the rule tables the `verified` badge wore a checkmark ("✓ Verified") while `grounded`
was distinguished only by a blue tint with no symbol — too faint as a status, and not visually
distinct enough from the symbol-less draft / needs-re-check badges.

**Fix.** Grounded now carries its own circled source-dot glyph, `⦿ Grounded` (`\u{29bf}`), across
all three table badge maps and the modal `verif_badge` helper — a clear status distinct from the
verified checkmark and the other badges. CSS for `.verif-badge-grounded` gets a more saturated
border so the glyph reads at the 10px table size. A unit test asserts the grounded label contains
the source-dot glyph and does NOT reuse the verified checkmark.

## Tests

All `camerata-ui` tests pass (49, incl. 6 new: 3 `selection_key`, deep-tier cost, incremental-flag
cost, hardened grounded-badge). `camerata-rules` stays green (50 + 1 doc-test). `cargo check -p
camerata-ui` is green.
