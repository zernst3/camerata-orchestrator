# Baseline / ratchet + auditable suppressions

Status: accepted (2026-06-16); engine built + tested, arm-writes-baseline + registry UI next.

## Context

The make-or-break brownfield decision. A real legacy repo has hundreds of pre-existing
violations. If onboarding made the gate block all new work until that debt was fixed,
no team would adopt it — you would red-flag their whole codebase and freeze them. Every
tool that retrofits rules onto legacy code (eslint, ruff, mypy, sonar) solves this with
a baseline / ratchet.

## Decision: report everything, enforce on the delta

Camerata **reports** all existing violations (for fix / waive / debt-story triage) but
**enforces** only on NEW or CHANGED code. A violation that is suppressed (baselined or
waived) does not block; a new one does. The deny-before-execute gate already only sees
new writes, so it ratchets naturally; the baseline is what makes the whole-repo CI gate
ratchet instead of freeze.

Each finding carries a `status`: `active` (enforced), `suppressed-inline`,
`suppressed-baseline`. The scan classifies every finding; the report shows all with the
status visible (an "Enforced / Baseline debt / Waived" badge + an enforced-vs-suppressed
summary).

## Two homes for two kinds of exception

- **Inline waiver** — a per-line, surgical exception co-located with the code:
  `// camerata:allow RULE-ID -- reason [, TICKET]`. It shows up in the PR diff (silencing
  a rule becomes a reviewable, challengeable act), `git blame` gives who/when for free,
  and it travels through refactors. The linter model.
- **Central baseline** — bulk / legacy / policy exceptions in `.camerata/baseline.json`
  with metadata (rule, path, fingerprint, reason, accepted_by, accepted_at, kind, ticket).
  The onboarding snapshot of pre-existing debt lives here, NOT as hundreds of scattered
  comments. So does an org-wide "waive rule X until Q3" policy.

## Three governance invariants (regardless of home)

1. **Reason required, gated.** A reason-less `camerata:allow` is itself a violation
   (`CAM-WAIVER-NEEDS-REASON`) and does not suppress anything — the un-auditable hole
   this mechanism exists to prevent.
2. **Indexed centrally.** Inline waivers roll up (with the baseline) into one queryable
   registry, so "show me everything we've waived, by rule / age / who" is a lookup, not a
   grep. Store inline; roll up centrally.
3. **Stale ones surfaced.** A waiver whose violation no longer exists is a dead directive
   silently masking future violations. A period scan flags inline + baseline suppressions
   that match no live finding, for removal.

## Fingerprinting (the ratchet mechanics)

Baseline entries match by a content fingerprint, not a line number: a stable,
dependency-free FNV-1a hash of `rule_id` + the whitespace-normalized offending snippet.
So a suppression survives line drift (code above it shifts), but the moment the offending
code ITSELF changes, the fingerprint no longer matches → it counts as new/changed →
enforced. Touching debt un-baselines it. That is the ratchet tightening.

## Tie-back: ignore and debt-story are one act

A waiver can carry its tracked ticket (`-- accepted as debt, JIRA-123`), linking the
inline suppression to the tracked story, so "ignore" and "create a story" are one
auditable act, not two disconnected ones.

## Surface

`suppression.rs` (pure, fully unit-tested): `InlineWaiver` + `parse_inline_waivers`,
`Baseline`/`BaselineEntry`, `fingerprint`, `classify_one` (active / suppressed-inline /
suppressed-baseline), `reasonless_waivers`, `stale_inline` / `stale_baseline`, `registry`
(unified audit view with stale flags). Wired into the scan (`classify_repo_findings`):
parses inline waivers from the files, loads the committed `.camerata/baseline.json`, sets
each finding's status, and surfaces reason-less waivers as violations. UI shows the
status badge + enforced-vs-suppressed counts.

## Next

- **Arm writes the baseline**: on onboarding, snapshot the current active findings into
  `.camerata/baseline.json` (the bulk accept), so the team is unblocked immediately.
- **The CI gate reads the baseline** and fails only on non-baselined (new) violations.
- **Suppression registry endpoint + view**: the central "everything we've waived" audit
  surface, with the stale flags.
- **Ignore-with-reason action** in the report (writes an inline waiver or baseline entry).
