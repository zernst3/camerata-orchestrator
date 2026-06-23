# Decision: Scan Preview Floor / Semgrep Deduplication

**Date:** 2026-06-22
**Status:** Implemented (`fix/scan-floor-semgrep-dedup`)
**Files changed:** `crates/server/src/lib.rs`

## Problem

The scan preview runs TWO independent detectors over the same repo:

1. The **deterministic floor** (`audit_content` in `onboard.rs`, rules `SEC-NO-HARDCODED-SECRETS-1`, `SEC-NO-RAW-SQL-CONCAT-1`, `ARCH-NO-SECRETS-IN-URL-1`) — pure regex via `camerata_gateway::content_match_lines`.
2. **Semgrep** (`crates/server/src/scan_tools.rs`, bundled ruleset `crates/server/assets/semgrep-rules/security.yml`, 10 rules).

Two of the 10 semgrep rules overlap the floor on the same `(repo, path, line)` axis:

| Semgrep rule id | Floor rule id |
|---|---|
| `camerata.security.hardcoded-secret` | `SEC-NO-HARDCODED-SECRETS-1` |
| `camerata.security.sql-string-concat-python` | `SEC-NO-RAW-SQL-CONCAT-1` |
| `camerata.security.sql-string-concat-js` | `SEC-NO-RAW-SQL-CONCAT-1` |

Before this fix, `merge_scan_preview` in `lib.rs` did a raw `Vec::append` — so a secret on line 7 of `src/config.rs` produced TWO finding rows in the scan preview: one with `SEC-NO-HARDCODED-SECRETS-1` (floor) and one with `camerata.security.hardcoded-secret` (semgrep).

## Why BOTH rulesets stay intact

The two detectors enforce at DIFFERENT layers:

- **Floor** enforces at Layer 1 (the git-push gate) AND at scan preview. It is the always-on, non-deselectable, enforcement boundary. `eval.rs` scores by floor rule IDs; the gate blocks on `SEC-*` rule IDs.
- **Semgrep** enforces at scan preview AND CI (Layer 2 / Layer 3). Trimming the overlapping semgrep rules would punch a hole in Layer 2/3 CI coverage — repos that have not wired the floor gate would lose detection entirely.

**The fix is presentation-time dedup at scan preview only**, not rule removal.

## Overlap mapping

The mapping is explicit and exact — no fuzzy heuristics. Codified in `semgrep_floor_category(rule_id) -> Option<&'static str>` in `lib.rs`. The remaining 7 semgrep rules (`exec-injection`, `exec-injection-js`, `weak-hash-python`, `weak-hash-js`, `path-traversal-python`, `subprocess-shell-true`, `yaml-unsafe-load`) return `None` — they have no floor twin and pass through untouched.

## Why floor is canonical

When a duplicate is detected, the floor finding is kept and the semgrep finding is dropped. Rationale:

1. The `SEC-*` rule IDs are what `eval.rs` scores. Swapping to semgrep rule IDs would silently break gate scoring for findings that flow into enforcement.
2. Floor findings have `preview: false` (enforced). Semgrep preview findings have `preview: true` (advisory-but-deterministic). Keeping the enforced finding as canonical is the honest choice.
3. Provenance is not lost: the semgrep rule ID is appended to `also_matches` on the kept floor finding. The row honestly records "violates `SEC-NO-HARDCODED-SECRETS-1`, also flagged by `camerata.security.hardcoded-secret`."

## Exact-line dedup scope

Dedup uses exact `usize ==` line equality. A semgrep finding on line 5 and a floor finding on line 6 are NOT duplicates — adjacent lines can legitimately both have a problem, and fuzzy proximity matching would silently drop real findings. This is intentionally conservative: a false-kept row is cheaper than a silently-dropped real finding (see `camerata_scan_output_over_tell_preference` in memory/feedback).

## Implementation

`dedup_preview_against_floor(existing: &mut Vec<Finding>, previews: Vec<Finding>) -> Vec<Finding>` in `lib.rs`:

- Iterates the batch of preview findings from `run_scan_tools`.
- For each semgrep finding whose rule maps to a floor rule: looks for an existing floor finding at the exact `(repo, path, line, floor_rule_id)`. If found: appends the semgrep rule ID to `also_matches` on the floor finding and drops the semgrep copy. If not found (line mismatch, net-new coverage): keeps the semgrep finding.
- Non-semgrep tool findings and semgrep findings for non-overlapping rules pass through unconditionally.

Called in `merge_scan_preview` replacing the former raw `Vec::append`.

## Alternatives considered

- **Remove overlapping rules from semgrep**: Rejected. Creates Layer 2/3 coverage hole.
- **Remove overlapping rules from the floor and use semgrep as canonical**: Rejected. Breaks `eval.rs` scoring and the Layer-1 gate; changes the enforcement contract.
- **Fuzzy line proximity (±N lines)**: Rejected. Too many silent false-drops; exact equality is safer and sufficient for the real-world case (both detectors fire on the same line).
- **Add a new field to `Finding` instead of `also_matches`**: Rejected. `also_matches` already carries exactly this semantics ("other rule IDs that fired at the same location"). No new field needed; no `#[serde(default)]` gap.
